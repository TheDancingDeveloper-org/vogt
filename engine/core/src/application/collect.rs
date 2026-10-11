//! Running the collectors. Ports `sweep` and the dependency-reference
//! resolution from `services/collect.py`.
//!
//! Scope is always the registered project list, optionally narrowed to one
//! project and never widened by looking around. An unnamed sweep runs the core
//! collectors that are not on-demand: dep-refs, git-local, source-markers and
//! mirrored-source, plus session-outcomes when an engine is configured and
//! agent-activity when transcript roots are. Contract-checker runs only when
//! named. Forge and deploy-lane collectors are not ported: a sweep refuses
//! while either is configured rather than answering as if they did not exist.
//! CI watch is not started here.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Component, Path, PathBuf};

use serde_json::{json, Map, Value};

use crate::application::context::{AppContext, Built};
use crate::application::resolve;
use crate::collectors::{
    AgentActivityCollector, CollectorContext, CollectorRegistry, DeclaredSessions,
    MirroredSourceCollector, Registered, RegisteredProject, SessionOutcomeCollector, SessionRecord,
    Sweeper, SESSION_OBSERVATION_LIMIT,
};
use crate::core::{Clock, CodingSession, IdFactory, Observation, Project};
use crate::errors::VogtError;
use crate::storage::interface::{DeclaredStore, ObservedStore, ReadView};
use crate::storage::observed_types::{DepRefRow, SweepReport};

/// `collect.py` `SWEEP_COMPLETED_EVENT`.
const SWEEP_COMPLETED_EVENT: &str = "sweep.completed";

/// `dep_refs.py` `KIND_DEP_REF`. No collector emits it yet; the resolution
/// still has to know the kind so a planted observation resolves the same way.
const KIND_DEP_REF: &str = "dep_ref";

/// `collect.py` `_GIT_SUFFIX`.
const GIT_SUFFIX: &str = ".git";

/// Collect over the registered projects, then rebuild the projections.
pub fn sweep_op(ctx: &Built, params: Value) -> Result<Value, VogtError> {
    crate::with_ctx!(ctx, |ctx| sweep(ctx, &params))
}

fn sweep<C: Clock + 'static, I: IdFactory + 'static>(
    ctx: &AppContext<C, I>,
    params: &Value,
) -> Result<Value, VogtError> {
    let project_slug = optional_string(params, "project");
    let named = string_list(params, "collectors");
    let offline_only = params
        .get("offline_only")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    // Forge and deploy-lane collectors are not ported. Python registers them
    // whenever one is configured, so a sweep that quietly omitted them would
    // answer differently from Python on every real deployment that has one.
    if crate::adapters::forge::has_configured_forge(&ctx.config) {
        return Err(VogtError::InvalidRequest(
            "sweep cannot run while a forge is configured: the forge collectors (forge-issues, forge-prs, forge-checks, forge-releases, forge-posture, forge-notifications, forge-labels) are not ported yet"
                .to_string(),
        ));
    }
    if !ctx.config.deploy_lanes.is_empty() {
        return Err(VogtError::InvalidRequest(
            "sweep cannot run while a deploy lane is configured: the deploy-lanes collector is not ported yet"
                .to_string(),
        ));
    }

    let projects = {
        let view = ctx.declared.read()?;
        if let Some(slug) = project_slug.as_deref() {
            vec![resolve::project(&view, slug)?]
        } else {
            view.list_projects(10_000, 0)?
        }
    };

    let mut registry = CollectorRegistry::new(None).map_err(VogtError::InvalidRequest)?;
    // mirrored-source matches a path member against every registered project's
    // root manifest, so a sweep narrowed to one slug still has to see them all.
    let registered = {
        let view = ctx.declared.read()?;
        view.list_projects(10_000, 0)?
            .into_iter()
            .map(|project| RegisteredProject {
                id: project.id,
                slug: project.slug,
                root_path: project.root_path,
                repo_url: project.repo_url,
            })
            .collect()
    };
    registry
        .add(Registered::Collector(Box::new(
            MirroredSourceCollector::new(registered),
        )))
        .map_err(VogtError::InvalidRequest)?;
    if let Some(engine) = ctx.engine.clone() {
        registry
            .add(Registered::Collector(Box::new(
                SessionOutcomeCollector::new(
                    engine,
                    DeclaredSessionJoin::new(ctx.declared.clone()),
                ),
            )))
            .map_err(VogtError::InvalidRequest)?;
    }
    if !ctx.config.agent_activity_roots.is_empty() {
        let services: Vec<(&str, &str)> = ctx
            .config
            .agent_activity_services
            .iter()
            .map(|(pattern, service)| (pattern.as_str(), service.as_str()))
            .collect();
        let indexer = AgentActivityCollector::new(
            ctx.config.agent_activity_roots.clone(),
            ctx.config.agent_activity_max_bytes_per_sweep,
            &services,
        )
        .map_err(VogtError::InvalidRequest)?;
        registry
            .add(Registered::Indexer(Box::new(indexer)))
            .map_err(VogtError::InvalidRequest)?;
    }
    let mut selected: Vec<String> = registry
        .select(&named, offline_only)?
        .into_iter()
        .map(|collector| collector.name().to_string())
        .collect();
    if project_slug.is_some() && named.is_empty() {
        // A sweep narrowed to one project is never widened: an indexer reads
        // roots, not projects, so it runs only when named.
        selected.retain(|name| {
            registry
                .get(name)
                .is_ok_and(|collector| collector.project_scoped())
        });
    }
    if selected.is_empty() {
        let available = registry.names().join(", ");
        return Err(VogtError::InvalidRequest(format!(
            "no collectors selected (available: {})",
            if available.is_empty() {
                "none"
            } else {
                &available
            }
        )));
    }
    let indexers: HashSet<String> = selected
        .iter()
        .filter(|name| {
            registry
                .get(name)
                .is_ok_and(|collector| !collector.project_scoped())
        })
        .cloned()
        .collect();
    let collectors = registry.take(&selected)?;

    let mut clock = ctx
        .clock
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let reports = {
        let context = CollectorContext {
            config: &ctx.config,
            options: Map::new(),
        };
        Sweeper::new(&ctx.observed, context, &mut *clock).run(collectors, &projects)?
    };

    // An indexer writes its own tables, not observations, so what it indexed
    // never calls for the projections to be rebuilt.
    let total_new: i64 = reports
        .iter()
        .filter(|report| !indexers.contains(&report.collector))
        .map(|report| report.new)
        .sum();
    let (subjects, dep_refs) = match projections(ctx, total_new) {
        Ok(counts) => counts,
        Err(error) => {
            // Every collector above already committed its own sweep row. Left
            // alone, coverage would keep reporting this batch fresh from a run
            // nothing downstream ever heard complete.
            let detail = format!(
                "sweep batch did not complete: {}: {}",
                error.type_name(),
                error.message()
            );
            let ids: Vec<String> = reports
                .iter()
                .map(|report| report.sweep_id.clone())
                .collect();
            ctx.observed.fail_sweeps(&ids, &detail)?;
            return Err(error);
        }
    };

    for report in &reports {
        ctx.declared.publish_event(
            SWEEP_COMPLETED_EVENT,
            "sweep",
            &report.sweep_id,
            &report_summary(report),
            clock.now(),
        )?;
    }
    drop(clock);

    Ok(json!({
        "scope": project_slug.unwrap_or_else(|| "all registered projects".to_string()),
        "projects": projects.len(),
        "subjects": subjects,
        "dep_refs": dep_refs,
        "reports": reports.iter().map(report_view).collect::<Vec<_>>(),
    }))
}

/// Rebuild both projections, or read the counts when nothing new landed.
fn projections<C: Clock, I: IdFactory>(
    ctx: &AppContext<C, I>,
    total_new: i64,
) -> Result<(i64, i64), VogtError> {
    if total_new == 0 {
        // Digest dedup appended nothing, so both projections are already
        // correct — skip the delete-and-reinsert of every subject and dep ref.
        let current = ctx.observed.counts()?;
        return Ok((
            count_of(&current, "subjects"),
            count_of(&current, "dep_refs"),
        ));
    }
    let subjects = ctx.observed.rebuild_latest()?;
    let rows = resolve_dep_refs(ctx)?;
    ctx.observed.replace_dep_refs(&rows)?;
    Ok((subjects, i64::try_from(rows.len()).unwrap_or(i64::MAX)))
}

fn count_of(counts: &BTreeMap<String, i64>, key: &str) -> i64 {
    counts.get(key).copied().unwrap_or(0)
}

/// The declared reads `session-outcomes` needs, done on its behalf.
///
/// Collectors never touch declared data. This holds another handle on the same
/// store and re-reads per project, the way Python's `_DeclaredSessions` does.
struct DeclaredSessionJoin<C, I> {
    declared: crate::storage::sqlite::declared::SqliteDeclaredStore<C, I>,
    work_item_projects: std::cell::RefCell<HashMap<String, Option<String>>>,
}

impl<C: Clock, I: IdFactory> DeclaredSessionJoin<C, I> {
    fn new(declared: crate::storage::sqlite::declared::SqliteDeclaredStore<C, I>) -> Self {
        Self {
            declared,
            work_item_projects: std::cell::RefCell::new(HashMap::new()),
        }
    }
}

impl<C: Clock, I: IdFactory> DeclaredSessions for DeclaredSessionJoin<C, I> {
    fn for_project(&self, project: &Project) -> Vec<SessionRecord> {
        let Ok(view) = self.declared.read() else {
            return Vec::new();
        };
        let Ok(sessions) = view.list_sessions(
            Some(&project.id),
            None,
            true,
            i64::try_from(SESSION_OBSERVATION_LIMIT).unwrap_or(i64::MAX),
            0,
        ) else {
            return Vec::new();
        };
        let mut refs: HashMap<String, String> = HashMap::new();
        for session in &sessions {
            let Some(work_item_id) = session.work_item_id.as_deref() else {
                continue;
            };
            if refs.contains_key(work_item_id) {
                continue;
            }
            if let Ok(Some(item)) = view.work_item_by_id(work_item_id) {
                refs.insert(work_item_id.to_string(), item.reference);
            }
        }
        sessions
            .iter()
            .map(|session| session_record(session, &refs))
            .collect()
    }

    fn project_of_work_item(&self, work_item_ref: &str) -> Option<String> {
        if let Some(cached) = self.work_item_projects.borrow().get(work_item_ref) {
            return cached.clone();
        }
        let slug = self.declared.read().ok().and_then(|view| {
            let item = view.work_item_by_ref(work_item_ref).ok().flatten()?;
            let project_id = item.project_id.as_deref()?;
            view.project_by_id(project_id)
                .ok()
                .flatten()
                .map(|project| project.slug)
        });
        self.work_item_projects
            .borrow_mut()
            .insert(work_item_ref.to_string(), slug.clone());
        slug
    }
}

fn session_record(session: &CodingSession, refs: &HashMap<String, String>) -> SessionRecord {
    SessionRecord {
        id: session.id.clone(),
        engine_session_id: session.engine_session_id.clone(),
        cwd: session.cwd.clone(),
        started_at: session.started_at,
        stopped_at: session.stopped_at,
        work_item: session
            .work_item_id
            .as_deref()
            .and_then(|id| refs.get(id).cloned()),
    }
}

fn report_summary(report: &SweepReport) -> Value {
    json!({
        "collector": report.collector,
        "outcome": report.outcome,
        "projects": report.projects,
        "new": report.new,
        "unchanged": report.unchanged,
    })
}

fn report_view(report: &SweepReport) -> Value {
    json!({
        "collector": report.collector,
        "sweep_id": report.sweep_id,
        "outcome": report.outcome,
        "projects": report.projects,
        "new": report.new,
        "unchanged": report.unchanged,
        "failures": report.failures,
    })
}

// -- dependency resolution ----------------------------------------------------

/// How a raw reference target is matched to a registered project.
struct ResolutionIndex {
    by_path: HashMap<String, String>,
    by_repo: HashMap<String, String>,
    roots: HashMap<String, PathBuf>,
}

fn index(projects: &[Project]) -> ResolutionIndex {
    let mut by_path = HashMap::new();
    let mut by_repo = HashMap::new();
    let mut roots = HashMap::new();
    for project in projects {
        let root = expand_user(Path::new(&project.root_path));
        let key = if root.exists() {
            resolve_like_python(&root)
        } else {
            root.clone()
        };
        by_path.insert(key.to_string_lossy().into_owned(), project.id.clone());
        if let Some(repo_url) = project.repo_url.as_deref() {
            by_repo.insert(normalise_repo(repo_url), project.id.clone());
        }
        roots.insert(project.id.clone(), root);
    }
    ResolutionIndex {
        by_path,
        by_repo,
        roots,
    }
}

/// Reduce a repository URL to `host/owner/repo`.
///
/// The same repository is written half a dozen ways, and a reference that
/// fails to resolve because of punctuation is worse than no graph at all.
fn normalise_repo(url: &str) -> String {
    let mut candidate = url
        .trim()
        .strip_prefix("git+")
        .unwrap_or(url.trim())
        .to_string();
    if let Some((host, path)) = scp_style(&candidate) {
        candidate = format!("{host}/{path}");
    } else {
        for prefix in ["https://", "http://", "ssh://", "github:"] {
            if let Some(rest) = candidate.strip_prefix(prefix) {
                candidate = rest.to_string();
                break;
            }
        }
    }
    if let Some(stripped) = candidate.strip_suffix(GIT_SUFFIX) {
        candidate = stripped.to_string();
    }
    candidate.trim_matches('/').to_lowercase()
}

/// `collect.py` `_SCP_STYLE`: `git@host:path` or `git@host/path`, with the
/// optional `git+` and `ssh://` already stripped by the caller.
fn scp_style(candidate: &str) -> Option<(&str, &str)> {
    let rest = candidate.strip_prefix("ssh://").unwrap_or(candidate);
    let rest = rest.strip_prefix("git@")?;
    let split = rest.find([':', '/'])?;
    let host = &rest[..split];
    let path = &rest[split + 1..];
    if host.is_empty() || path.is_empty() || host.contains('/') {
        return None;
    }
    Some((host, path))
}

/// Turn dependency observations into the resolved projection.
fn resolve_dep_refs<C: Clock, I: IdFactory>(
    ctx: &AppContext<C, I>,
) -> Result<Vec<DepRefRow>, VogtError> {
    let projects = ctx.declared.read()?.list_projects(10_000, 0)?;
    let index = index(&projects);
    let mut rows = Vec::new();
    for observation in
        ctx.observed
            .latest(&[KIND_DEP_REF.to_string()], None, false, false, 100_000)?
    {
        if let Some(row) = dep_ref_row(&index, &observation) {
            rows.push(row);
        }
    }
    Ok(rows)
}

fn dep_ref_row(index: &ResolutionIndex, observation: &Observation) -> Option<DepRefRow> {
    let from_project_id = observation.project_id.clone()?;
    let ref_kind = payload_string(&observation.payload, "ref_kind", "path");
    let raw_target = payload_string(&observation.payload, "raw_target", "");
    let manifest = observation.payload.get("manifest").and_then(json_string);
    let resolved = resolve_target(
        index,
        &from_project_id,
        &ref_kind,
        &raw_target,
        manifest.as_deref(),
    );
    Some(DepRefRow {
        subject_key: observation.subject_key.clone(),
        from_project_id,
        ref_kind,
        raw_target,
        manifest,
        to_project_id: resolved,
        observed_at: observation.observed_at,
    })
}

/// Resolve one reference, or leave it unresolved with its raw target.
///
/// An internal-looking reference that resolves to nothing is kept as-is: it is
/// usually a project nobody registered yet, which is the thing worth surfacing.
fn resolve_target(
    index: &ResolutionIndex,
    from_project_id: &str,
    ref_kind: &str,
    raw_target: &str,
    manifest: Option<&str>,
) -> Option<String> {
    if ref_kind == "git" {
        return index.by_repo.get(&normalise_repo(raw_target)).cloned();
    }
    let root = index.roots.get(from_project_id)?;
    let base = match manifest {
        Some(manifest) => root.join(Path::new(manifest).parent().unwrap_or(Path::new(""))),
        None => root.clone(),
    };
    let mut target = raw_target.to_string();
    for prefix in ["file:", "link:", "portal:", "workspace:"] {
        if let Some(rest) = target.strip_prefix(prefix) {
            target = rest.to_string();
        }
    }
    if target.is_empty() || target == "." {
        return None;
    }
    let candidate = resolve_like_python(&base.join(&target));
    index
        .by_path
        .get(candidate.to_string_lossy().as_ref())
        .cloned()
}

/// `Path.resolve()`: canonicalise the longest prefix that exists and normalise
/// the remainder, so a tail that does not exist still loses its `..`.
fn resolve_like_python(path: &Path) -> PathBuf {
    let mut existing = path.to_path_buf();
    let mut missing = Vec::new();
    while !existing.as_os_str().is_empty() && !existing.exists() {
        match existing.file_name() {
            Some(name) => missing.push(PathBuf::from(name)),
            None => break,
        }
        if !existing.pop() {
            break;
        }
    }
    let mut resolved = if existing.as_os_str().is_empty() {
        PathBuf::new()
    } else {
        std::fs::canonicalize(&existing).unwrap_or(existing)
    };
    for part in missing.iter().rev() {
        resolved.push(part);
    }
    normalize(resolved)
}

/// Collapse `.` and `..` without touching the filesystem.
fn normalize(path: PathBuf) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// `pathlib.Path.expanduser`. `~` is this user's home; `~other` stays written.
fn expand_user(path: &Path) -> PathBuf {
    let mut components = path.components();
    let Some(Component::Normal(first)) = components.next() else {
        return path.to_path_buf();
    };
    if first.to_string_lossy() == "~" {
        if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
            return home.join(components.as_path());
        }
    }
    path.to_path_buf()
}

fn payload_string(payload: &Value, key: &str, default: &str) -> String {
    payload
        .get(key)
        .and_then(json_string)
        .unwrap_or_else(|| default.to_string())
}

/// Python's `str(value)` for the payload fields the resolution reads: a JSON
/// string as itself, anything else stringified the way `str` would.
fn json_string(value: &Value) -> Option<String> {
    match value {
        Value::Null => None,
        Value::String(text) => Some(text.clone()),
        Value::Bool(flag) => Some(flag.to_string()),
        Value::Number(number) => Some(number.to_string()),
        other => Some(other.to_string()),
    }
}

fn optional_string(params: &Value, key: &str) -> Option<String> {
    match params.get(key) {
        Some(Value::String(text)) if !text.is_empty() => Some(text.clone()),
        _ => None,
    }
}

fn string_list(params: &Value, key: &str) -> Vec<String> {
    params
        .get(key)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// The variant's name, which is what Python's `type(exc).__name__` reports.
trait ErrorName {
    fn type_name(&self) -> &'static str;
}

impl ErrorName for VogtError {
    fn type_name(&self) -> &'static str {
        match self {
            Self::NotInitialized(_) => "NotInitialized",
            Self::AlreadyInitialized(_) => "AlreadyInitialized",
            Self::NotFound(_) => "NotFound",
            Self::Conflict(_) => "Conflict",
            Self::BypassRefused(_) => "BypassRefused",
            Self::RoleRefused(_) => "RoleRefused",
            Self::GrantRefused(_) => "GrantRefused",
            Self::PersonRequired(_) => "PersonRequired",
            Self::EngineOnly(_) => "EngineOnly",
            Self::InvalidRequest(_) => "InvalidRequest",
            Self::InstallClosed(_) => "InstallClosed",
            Self::LoginThrottled(_) => "LoginThrottled",
            Self::InvalidCursor(_) => "InvalidCursor",
            Self::ForgeAccountsNotConfigured(_) => "ForgeAccountsNotConfigured",
            Self::ImportParityRefused(_) => "ImportParityRefused",
            Self::ImportWorkingTreeDirty(_) => "ImportWorkingTreeDirty",
            Self::ImportBranchDiverged(_) => "ImportBranchDiverged",
            Self::PublishRefused(_) => "PublishRefused",
            Self::PublishSourceInvalid(_) => "PublishSourceInvalid",
            Self::PublishWorkingTreeDirty(_) => "PublishWorkingTreeDirty",
            Self::PublishNonFastForward(_) => "PublishNonFastForward",
            Self::RemoteRepoExists(_) => "RemoteRepoExists",
            Self::NotLinked(_) => "NotLinked",
            Self::LinkRefused(_) => "LinkRefused",
            Self::UpstreamWriteRefused(_) => "UpstreamWriteRefused",
            Self::UpstreamWriteFailed(_) => "UpstreamWriteFailed",
            Self::InboxEntryNotFound(_) => "InboxEntryNotFound",
            Self::InvalidTriageState(_) => "InvalidTriageState",
            Self::PreferenceVersionConflict(_) => "PreferenceVersionConflict",
            Self::InvalidPreference(_) => "InvalidPreference",
            Self::InvalidSnooze(_) => "InvalidSnooze",
            Self::MissingReason(_) => "MissingReason",
            Self::InvalidParams(_) => "InvalidParams",
            Self::MigrationError(_) => "MigrationError",
            Self::MigrationLocked(_) => "MigrationLocked",
            Self::EngineUnavailable(_) => "EngineUnavailable",
            Self::GitUnavailable(_) => "GitUnavailable",
            Self::GitCommandFailed(_) => "GitCommandFailed",
            Self::GitHubUnavailable(_) => "GitHubUnavailable",
            Self::PeerUnavailable { .. } => "PeerUnavailable",
            Self::ForgejoUnavailable(_) => "ForgejoUnavailable",
            Self::TransitionRejected { .. } => "TransitionRejected",
            Self::Unauthenticated(_) => "Unauthenticated",
            Self::Forbidden(_) => "Forbidden",
            Self::CollectorFailed(_) => "CollectorFailed",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::context::{build_context, Built};
    use crate::application::projects::register_op;
    use crate::application::services::history::events_list_op;
    use crate::config::VogtConfig;
    use crate::core::{ActorKind, Moment, Principal, SequentialIds, StepClock};
    use crate::storage::observed_types::PendingObservation;

    fn moment() -> Moment {
        Moment::from_unix(1_700_000_000, 0)
    }

    fn unique() -> u64 {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        COUNTER.fetch_add(1, Ordering::Relaxed)
    }

    fn opened() -> Built {
        let dir =
            std::env::temp_dir().join(format!("vogt-collect-{}-{}", std::process::id(), unique()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut clock = Some(StepClock::new(moment()));
        let mut ids = Some(SequentialIds::new(None).unwrap());
        crate::application::instance::init(&dir, &mut clock, &mut ids).unwrap();
        let config = VogtConfig {
            data_dir: dir,
            ..VogtConfig::default()
        };
        build_context(
            config,
            Some(Principal::new("local:test-user", ActorKind::Human, "Test").unwrap()),
            clock,
            ids,
            None,
            None,
            None,
            None,
        )
        .unwrap()
    }

    fn register(built: &Built, name: &str, root: &Path, repo: Option<&str>) {
        let mut params = json!({
            "name": name,
            "root_path": root.to_string_lossy(),
            "reason": "parity fixture",
        });
        if let Some(repo) = repo {
            params["repo_url"] = Value::String(repo.to_string());
        }
        register_op(built, params).unwrap();
    }

    /// A configured forge or deploy lane refuses the whole sweep, and the
    /// refusal names the collectors that are not ported yet.
    #[test]
    fn a_configured_forge_or_deploy_lane_refuses_the_sweep() {
        let dir = std::env::temp_dir().join(format!(
            "vogt-collect-refuse-{}-{}",
            std::process::id(),
            unique()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let token = dir.join("forge-token");
        std::fs::write(&token, "secret-token\n").unwrap();
        let config = VogtConfig {
            data_dir: dir.clone(),
            github_token_file: Some(token),
            ..VogtConfig::default()
        };
        let forged = build_context(
            config,
            Some(Principal::new("local:test-user", ActorKind::Human, "Test").unwrap()),
            Some(StepClock::new(moment())),
            Some(SequentialIds::new(None).unwrap()),
            None,
            None,
            None,
            None,
        )
        .unwrap();
        let refused = sweep_op(&forged, json!({})).unwrap_err();
        let message = refused.to_string();
        assert!(matches!(refused, VogtError::InvalidRequest(_)), "{message}");
        for name in [
            "forge-issues",
            "forge-prs",
            "forge-checks",
            "forge-releases",
            "forge-posture",
            "forge-notifications",
            "forge-labels",
        ] {
            assert!(message.contains(name), "{message}");
        }

        let config = VogtConfig {
            data_dir: dir,
            deploy_lanes: vec![crate::config::DeployLane {
                name: "dev".to_string(),
                project: "alpha".to_string(),
                branch: "main".to_string(),
                receipt_repo: None,
                receipt_path: None,
                version_url: None,
            }],
            ..VogtConfig::default()
        };
        let laned = build_context(
            config,
            Some(Principal::new("local:test-user", ActorKind::Human, "Test").unwrap()),
            Some(StepClock::new(moment())),
            Some(SequentialIds::new(None).unwrap()),
            None,
            None,
            None,
            None,
        )
        .unwrap();
        let refused = sweep_op(&laned, json!({})).unwrap_err();
        let message = refused.to_string();
        assert!(message.contains("deploy-lanes"), "{message}");
    }

    /// An unnamed sweep over a project holding one `TODO(vogt)` runs
    /// source-markers, rebuilds the projections, and publishes exactly one
    /// `sweep.completed`. A second sweep finds the marker unchanged and reads
    /// the counts instead of rebuilding.
    #[test]
    fn unnamed_sweep_runs_source_markers_and_publishes() {
        let dir =
            std::env::temp_dir().join(format!("vogt-src-{}-{}", std::process::id(), unique()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("notes.rs"), "// TODO(vogt): port the sweeper\n").unwrap();

        let built = opened();
        register(&built, "alpha", &dir, None);

        let first = sweep_op(&built, json!({})).unwrap();
        assert_eq!(first["scope"], "all registered projects");
        assert_eq!(first["projects"], 1);
        // source-markers finds the one TODO; dep-refs finds nothing in a tree
        // with no manifest, but it always writes its scan record; git-local
        // finds the directory is not a checkout; mirrored-source finds nothing
        // because the tree declares no package.
        assert_eq!(first["subjects"], 3);
        assert_eq!(first["dep_refs"], 0);
        let reports = first["reports"].as_array().unwrap();
        assert_eq!(reports.len(), 4);
        let markers = reports
            .iter()
            .find(|report| report["collector"] == "source-markers")
            .unwrap();
        assert_eq!(markers["outcome"], "ok");
        assert_eq!(markers["new"], 1);
        assert_eq!(markers["unchanged"], 0);
        assert!(markers.get("detail").is_none());
        let deps = reports
            .iter()
            .find(|report| report["collector"] == "dep-refs")
            .unwrap();
        assert_eq!(deps["outcome"], "ok");
        assert_eq!(deps["new"], 1);

        let events = events_list_op(&built, json!({"limit": 100})).unwrap();
        let completed: Vec<_> = events["events"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|event| event["kind"] == "sweep.completed")
            .collect();
        assert_eq!(completed.len(), 4);
        assert_eq!(completed[0]["entity_kind"], "sweep");
        let marker_event = completed
            .iter()
            .find(|event| event["summary"]["collector"] == "source-markers")
            .unwrap();
        assert_eq!(marker_event["entity_id"], markers["sweep_id"]);
        assert_eq!(marker_event["summary"]["new"], 1);

        let second = sweep_op(&built, json!({})).unwrap();
        assert_eq!(second["subjects"], 3);
        assert_eq!(second["dep_refs"], 0);
        let second_markers = second["reports"]
            .as_array()
            .unwrap()
            .iter()
            .find(|report| report["collector"] == "source-markers")
            .unwrap();
        assert_eq!(second_markers["new"], 0);
        assert_eq!(second_markers["unchanged"], 1);
    }

    #[test]
    fn missing_collector_is_not_found() {
        let built = opened();
        let missing = sweep_op(&built, json!({"collectors": ["not-a-collector"]}));
        let Err(VogtError::NotFound(message)) = missing else {
            panic!("a collector that is not ported is not found, not silently skipped");
        };
        assert_eq!(
            message,
            "no collector named 'not-a-collector' (available: contract-checker, dep-refs, git-local, mirrored-source, source-markers)"
        );
    }

    /// Planted dep_ref observations resolve the way Python's sweep does: a git
    /// URL collapses to host/owner/repo, a relative path resolves against the
    /// manifest's directory, `~` expands, and a tail that does not exist still
    /// matches the registered root.
    #[test]
    fn dep_ref_resolution_matches_python() {
        let base =
            std::env::temp_dir().join(format!("vogt-deps-{}-{}", std::process::id(), unique()));
        let from_root = base.join("from");
        let to_root = base.join("libs").join("to");
        std::fs::create_dir_all(from_root.join("crates")).unwrap();
        std::fs::create_dir_all(&to_root).unwrap();

        let built = opened();
        register(
            &built,
            "from",
            &from_root,
            Some("https://github.com/acme/from.git"),
        );
        register(&built, "to", &to_root, Some("git@github.com:acme/to.git"));

        let Built::StepSequential(ctx) = &built else {
            unreachable!()
        };
        let projects = ctx.declared.read().unwrap().list_projects(10, 0).unwrap();
        let from_id = projects
            .iter()
            .find(|p| p.slug == "from")
            .unwrap()
            .id
            .clone();
        let to_id = projects.iter().find(|p| p.slug == "to").unwrap().id.clone();

        let plant = |key: &str, payload: Value| PendingObservation {
            kind: KIND_DEP_REF.to_string(),
            subject_key: key.to_string(),
            payload,
            content_digest: key.to_string(),
            project_id: Some(from_id.clone()),
            source_url: None,
            promoted: false,
        };
        let sweep = ctx.observed.begin_sweep("dep-refs", &[], moment()).unwrap();
        ctx.observed
            .append(
                &sweep.id,
                &[
                    plant(
                        "git",
                        json!({"ref_kind": "git", "raw_target": "git+ssh://git@github.com/acme/to.git", "manifest": "Cargo.toml"}),
                    ),
                    plant(
                        "path",
                        json!({"ref_kind": "path", "raw_target": "file:../../../libs/to", "manifest": "crates/app/Cargo.toml"}),
                    ),
                    plant(
                        "self",
                        json!({"raw_target": "."}),
                    ),
                ],
                moment(),
            )
            .unwrap();

        ctx.observed.rebuild_latest().unwrap();
        let rows = resolve_dep_refs(ctx).unwrap();
        let by_key: HashMap<_, _> = rows
            .iter()
            .map(|row| (row.subject_key.as_str(), row))
            .collect();
        assert_eq!(by_key["git"].to_project_id.as_deref(), Some(to_id.as_str()));
        assert_eq!(by_key["git"].ref_kind, "git");
        assert_eq!(
            by_key["path"].to_project_id.as_deref(),
            Some(to_id.as_str())
        );
        assert_eq!(
            by_key["path"].manifest.as_deref(),
            Some("crates/app/Cargo.toml")
        );
        assert_eq!(by_key["self"].ref_kind, "path");
        assert_eq!(by_key["self"].to_project_id, None);

        // `~` is only expanded when it is the whole path, which is how a
        // registered root is stored (`Path.expanduser` before `resolve`). Once
        // joined onto a base it is an ordinary name, so a target written as
        // `~/…` does not resolve and one written relative to the root does.
        let home = std::env::var("HOME").unwrap();
        let expanded = PathBuf::from(&home).join("vogt-tilde-target");
        let _ = std::fs::remove_dir_all(&expanded);
        std::fs::create_dir_all(&expanded).unwrap();
        let index = ResolutionIndex {
            by_path: HashMap::from([(
                resolve_like_python(&expanded)
                    .to_string_lossy()
                    .into_owned(),
                "prj_home".to_string(),
            )]),
            by_repo: HashMap::new(),
            roots: HashMap::from([("prj_from".to_string(), PathBuf::from(&home))]),
        };
        let written_as_tilde =
            resolve_target(&index, "prj_from", "path", "~/vogt-tilde-target", None);
        assert_eq!(written_as_tilde, None);
        let relative = resolve_target(&index, "prj_from", "path", "vogt-tilde-target", None);
        assert_eq!(relative.as_deref(), Some("prj_home"));
        let missing_tail = resolve_target(
            &index,
            "prj_from",
            "path",
            "vogt-tilde-target/does-not-exist",
            None,
        );
        assert_eq!(missing_tail, None);
        let _ = std::fs::remove_dir_all(&expanded);
    }
}
