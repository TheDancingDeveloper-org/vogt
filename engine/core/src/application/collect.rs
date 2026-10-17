//! Running the collectors. Ports `sweep` and the dependency-reference
//! resolution from `services/collect.py`.
//!
//! Scope is always the registered project list, optionally narrowed to one
//! project and never widened by looking around. An unnamed sweep runs the core
//! collectors that are not on-demand: dep-refs, git-local, source-markers and
//! mirrored-source, plus session-outcomes when an engine is configured,
//! agent-activity when transcript roots are, and when a forge is configured the
//! forge sync pair (forge-issues, forge-prs) and the five forge read collectors
//! (forge-checks, forge-releases, forge-labels, forge-posture,
//! forge-notifications), and deploy-lanes when a lane is configured. Contract-
//! checker runs only when named. Once the sweep has published its events, the
//! CI-watch announce runs after a checks collector, and its failure is logged,
//! never returned: a sweep whose evidence landed does not fail for a nudge.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
use std::rc::Rc;

use serde_json::{json, Value};

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
    let forge = crate::adapters::forge::has_configured_forge(&ctx.config)?;
    if forge || !ctx.config.deploy_lanes.is_empty() {
        // One directory serves the forge reads and the lanes' receipts, opened
        // once for the sweep, as Python's `forge_read_collectors` shares one.
        let directory = Rc::new(crate::adapters::forge::ForgeClients::open(
            ctx.config.clone(),
            ctx.forge_transport(),
        )?);
        if forge {
            register_forge(&mut registry, ctx, &directory)?;
        }
        if !ctx.config.deploy_lanes.is_empty() {
            // Registered only when a lane is configured, so an instance without
            // any reports deployed versions as "not configured", not "none".
            registry
                .add(Registered::Collector(Box::new(OwnedDeployLanes {
                    directory: Rc::clone(&directory),
                })))
                .map_err(VogtError::InvalidRequest)?;
        }
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

    // After the feed has the sweep: a bound branch whose CI just settled is
    // announced to its sessions. Contained, as Python contains it: a nudge that
    // cannot be delivered must never fail a sweep whose evidence already landed.
    if reports
        .iter()
        .any(|report| report.collector == crate::adapters::forge::COLLECTOR_CHECKS)
    {
        if let Err(error) = crate::application::ci_watch::announce_concluded(ctx) {
            tracing::warn!(
                target: "vogt.collect",
                error = %format!("{}: {}", error.type_name(), error.message()),
                "CI watch could not announce bound-branch conclusions"
            );
        }
    }

    Ok(json!({
        "scope": project_slug.unwrap_or_else(|| "all registered projects".to_string()),
        "projects": projects.len(),
        "subjects": subjects,
        "dep_refs": dep_refs,
        "disk": crate::application::instance::workspace_disk(ctx).unwrap_or(Value::Null),
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

/// Register the forge sync pair and the five forge read collectors, sharing one
/// directory opened once.
///
/// The directory resolves every configured host once at `open`, so no collector
/// re-reads a token file mid-sweep. Every collector holds the same `Rc`: the
/// registry stores owned collectors, and a directory cannot be borrowed by
/// several of them.
fn register_forge<C: Clock + 'static, I: IdFactory + 'static>(
    registry: &mut CollectorRegistry,
    ctx: &AppContext<C, I>,
    directory: &Rc<crate::adapters::forge::ForgeClients>,
) -> Result<(), VogtError> {
    let mut collectors = Vec::new();
    for kind in [ForgeSyncKind::Issues, ForgeSyncKind::Pulls] {
        collectors.push(Registered::Collector(Box::new(OwnedForgeSync::new(
            kind,
            ctx.observed.clone(),
            Rc::clone(directory),
        ))));
    }
    for kind in ForgeReadKind::ALL {
        collectors.push(owned_forge_read(ctx.observed.clone(), directory, kind));
    }
    for collector in collectors {
        registry.add(collector).map_err(VogtError::InvalidRequest)?;
    }
    Ok(())
}

/// The deploy-lanes collector as the sweep registry holds it: one observation
/// per configured lane, read through the sweep's shared directory and the real
/// version fetch.
pub(crate) struct OwnedDeployLanes {
    directory: Rc<crate::adapters::forge::ForgeClients>,
}

impl crate::collectors::Collector for OwnedDeployLanes {
    fn name(&self) -> &str {
        crate::adapters::forge::COLLECTOR_DEPLOY_LANES
    }

    fn requires_network(&self) -> bool {
        true
    }

    fn collect(
        &mut self,
        ctx: &mut CollectorContext<'_>,
        project: &crate::core::Project,
        _now: &mut dyn FnMut() -> crate::core::Moment,
    ) -> crate::collectors::Collection {
        use crate::adapters::forge::{DeployLanesCollector, HttpVersionFetch};
        let directory: &dyn crate::adapters::forge::ForgeDirectory = &*self.directory;
        let found: Result<_, VogtError> =
            Ok(DeployLanesCollector::new(directory, &HttpVersionFetch)
                .collect(ctx.config, project));
        found.into()
    }
}

/// Which provider read an [`OwnedForgeRead`] performs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ForgeReadKind {
    Checks,
    Releases,
    Labels,
    Posture,
    Notifications,
}

impl ForgeReadKind {
    /// Every read, in the order the sweep registers them.
    pub(crate) const ALL: [Self; 5] = [
        Self::Checks,
        Self::Releases,
        Self::Labels,
        Self::Posture,
        Self::Notifications,
    ];

    /// The collector name the sweep records and coverage lists.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Checks => crate::adapters::forge::COLLECTOR_CHECKS,
            Self::Releases => "forge-releases",
            Self::Labels => "forge-labels",
            Self::Posture => "forge-posture",
            Self::Notifications => crate::adapters::forge::COLLECTOR_NOTIFICATIONS,
        }
    }
}

/// One provider read, registered as an owned collector.
///
/// It holds a share of the directory, a handle on the observed store (the
/// previous observations the checks and notifications reads carry forward), and
/// the actor resolver notifications use. The borrowing collectors are built per
/// call, so the registry can hold this `'static` value.
pub(crate) struct OwnedForgeRead<C: Clock + 'static, I: IdFactory + 'static> {
    kind: ForgeReadKind,
    store: crate::storage::sqlite::observed::SqliteObservedStore<C, I>,
    directory: Rc<crate::adapters::forge::ForgeClients>,
    actors: crate::adapters::forge::ActorResolver,
}

/// The registry's form of one provider read.
pub(crate) fn owned_forge_read<C: Clock + 'static, I: IdFactory + 'static>(
    store: crate::storage::sqlite::observed::SqliteObservedStore<C, I>,
    directory: &Rc<crate::adapters::forge::ForgeClients>,
    kind: ForgeReadKind,
) -> Registered {
    Registered::Collector(Box::new(OwnedForgeRead {
        kind,
        store,
        directory: Rc::clone(directory),
        actors: crate::adapters::forge::ActorResolver::new(),
    }))
}

/// The previous observations of one kind, read from the observed store. The
/// collectors only read through this, so the store is borrowed, not owned.
struct StorePrior<'a, S: ObservedStore>(&'a S);

impl<S: ObservedStore> crate::adapters::forge::PriorObservations for StorePrior<'_, S> {
    fn latest(&self, kind: &str, project_id: &str, limit: i64) -> Vec<Observation> {
        self.0
            .latest(&[kind.to_owned()], Some(project_id), false, false, limit)
            .unwrap_or_default()
    }
}

impl<C: Clock + 'static, I: IdFactory + 'static> crate::collectors::Collector
    for OwnedForgeRead<C, I>
{
    fn name(&self) -> &str {
        self.kind.name()
    }

    fn requires_network(&self) -> bool {
        true
    }

    fn collect(
        &mut self,
        ctx: &mut CollectorContext<'_>,
        project: &crate::core::Project,
        _now: &mut dyn FnMut() -> crate::core::Moment,
    ) -> crate::collectors::Collection {
        use crate::adapters::forge::{
            ForgeChecksCollector, ForgeLabelsCollector, ForgeNotificationsCollector,
            ForgePostureCollector, ForgeReleasesCollector,
        };
        let directory: &dyn crate::adapters::forge::ForgeDirectory = &*self.directory;
        let prior = StorePrior(&self.store);
        let found = match self.kind {
            ForgeReadKind::Checks => {
                ForgeChecksCollector::new(directory, Some(&prior)).collect(ctx.config, project)
            }
            ForgeReadKind::Releases => ForgeReleasesCollector::new(directory).collect(project),
            ForgeReadKind::Labels => ForgeLabelsCollector::new(directory).collect(project),
            ForgeReadKind::Posture => ForgePostureCollector::new(directory).collect(project),
            ForgeReadKind::Notifications => {
                ForgeNotificationsCollector::new(directory, Some(&prior), &self.actors)
                    .collect(project)
            }
        };
        found.into()
    }
}

/// Which read an [`OwnedForgeSync`] performs.
pub(crate) enum ForgeSyncKind {
    Issues,
    Pulls,
}

/// The forge sync collector as the sweep registry holds it.
///
/// [`crate::adapters::forge::sync::ForgeSyncCollector`] borrows its store and
/// its directory, which is what its tests use. The registry keeps owned
/// `'static` collectors, so this owns both and calls the same reads and the
/// same post-append bookkeeping through them: another handle on the context's
/// observed store, and a share of the directory opened once for the sweep.
pub(crate) struct OwnedForgeSync<C: Clock + 'static, I: IdFactory + 'static> {
    name: &'static str,
    kind: ForgeSyncKind,
    store: crate::storage::sqlite::observed::SqliteObservedStore<C, I>,
    directory: Rc<crate::adapters::forge::ForgeClients>,
    pending: BTreeMap<String, crate::adapters::forge::PendingSync>,
}

impl<C: Clock + 'static, I: IdFactory + 'static> OwnedForgeSync<C, I> {
    pub(crate) fn new(
        kind: ForgeSyncKind,
        store: crate::storage::sqlite::observed::SqliteObservedStore<C, I>,
        directory: Rc<crate::adapters::forge::ForgeClients>,
    ) -> Self {
        let name = match kind {
            ForgeSyncKind::Issues => crate::adapters::forge::COLLECTOR_ISSUES,
            ForgeSyncKind::Pulls => crate::adapters::forge::COLLECTOR_PULLS,
        };
        Self {
            name,
            kind,
            store,
            directory,
            pending: BTreeMap::new(),
        }
    }
}

impl<C: Clock + 'static, I: IdFactory + 'static> crate::collectors::Collector
    for OwnedForgeSync<C, I>
{
    fn name(&self) -> &str {
        self.name
    }

    fn requires_network(&self) -> bool {
        true
    }

    fn collect(
        &mut self,
        _ctx: &mut CollectorContext<'_>,
        project: &crate::core::Project,
        _now: &mut dyn FnMut() -> crate::core::Moment,
    ) -> crate::collectors::Collection {
        let mut inner = match self.kind {
            ForgeSyncKind::Issues => {
                crate::adapters::forge::ForgeSyncCollector::issues(&self.store, &*self.directory)
            }
            ForgeSyncKind::Pulls => {
                crate::adapters::forge::ForgeSyncCollector::pulls(&self.store, &*self.directory)
            }
        };
        let found = inner.collect(project);
        self.pending.extend(
            inner
                .take_pending()
                .into_values()
                .map(|entry| (project.id.clone(), entry)),
        );
        found.into()
    }

    fn after_append(&mut self, at: crate::core::Moment) {
        // The trait returns `()` and the sweeper does not look at this. A
        // watermark that failed to advance must not read as advanced, so the
        // failure is logged rather than swallowed silently: the next sweep
        // re-reads the window, and digest dedup absorbs the replay.
        let pending = std::mem::take(&mut self.pending);
        if let Err(error) =
            crate::adapters::forge::commit_sync(&self.store, self.name, &pending, at)
        {
            tracing::warn!(
                target: "vogt.collect",
                "forge sync bookkeeping did not commit: collector={} error={}",
                self.name,
                error.message(),
            );
        }
    }

    fn post_append(&self) -> bool {
        true
    }
}
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

    /// A sweep of the forge read collectors goes through the stand-in: the
    /// labels and releases it names are read, the counts come back, and every
    /// request the sweep sent was a GET.
    #[test]
    fn a_named_forge_read_sweep_runs_through_the_stand_in_and_only_reads() {
        use crate::application::forge_standin::StandInForge;
        let dir = std::env::temp_dir().join(format!(
            "vogt-collect-standin-{}-{}",
            std::process::id(),
            unique()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let token = dir.join("forge-token");
        std::fs::write(&token, "secret-token\n").unwrap();
        let mut clock = Some(StepClock::new(moment()));
        let mut ids = Some(SequentialIds::new(None).unwrap());
        crate::application::instance::init(&dir, &mut clock, &mut ids).unwrap();
        let built = build_context(
            VogtConfig {
                data_dir: dir.clone(),
                github_token_file: Some(token),
                ..VogtConfig::default()
            },
            Some(Principal::new("local:test-user", ActorKind::Human, "Test").unwrap()),
            clock,
            ids,
            None,
            None,
            None,
            None,
        )
        .unwrap();
        let standin = StandInForge::one_of_each();
        let transport = standin.transport();
        let Built::StepSequential(ctx) = built else {
            panic!("expected a step clock");
        };
        let forged =
            Built::StepSequential(ctx.with_forge_transport(move |url, headers, body, method| {
                transport(url, headers, body, method)
            }));
        register(
            &forged,
            "widget",
            &dir,
            Some("https://github.com/acme/widget"),
        );

        let swept = sweep_op(
            &forged,
            json!({"project": "widget", "collectors": ["forge-labels", "forge-releases"]}),
        )
        .unwrap();
        let reports = swept["reports"].as_array().unwrap();
        for name in ["forge-labels", "forge-releases"] {
            let report = reports
                .iter()
                .find(|report| report["collector"] == name)
                .unwrap_or_else(|| panic!("{name} reported: {swept}"));
            assert_eq!(report["new"], 2, "one item and its receipt: {report}");
        }
        let requests = standin.requests();
        assert!(
            requests.iter().any(|request| request.contains("/labels?")),
            "{requests:?}"
        );
        assert!(
            requests
                .iter()
                .any(|request| request.contains("/releases?")),
            "{requests:?}"
        );
        assert!(
            requests.iter().all(|request| request.starts_with("GET ")),
            "a sweep only reads: {requests:?}"
        );
    }

    /// A configured forge no longer refuses the sweep: forge-issues and
    /// forge-prs run. A project with no repository is the unsupported receipt,
    /// which needs no network, so this stays offline.
    #[test]
    fn a_configured_forge_sweeps_its_sync_collectors_offline() {
        let dir = std::env::temp_dir().join(format!(
            "vogt-collect-forge-{}-{}",
            std::process::id(),
            unique()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let token = dir.join("forge-token");
        std::fs::write(&token, "secret-token\n").unwrap();
        let mut clock = Some(StepClock::new(moment()));
        let mut ids = Some(SequentialIds::new(None).unwrap());
        crate::application::instance::init(&dir, &mut clock, &mut ids).unwrap();
        let forged = build_context(
            VogtConfig {
                data_dir: dir.clone(),
                github_token_file: Some(token),
                ..VogtConfig::default()
            },
            Some(Principal::new("local:test-user", ActorKind::Human, "Test").unwrap()),
            clock,
            ids,
            None,
            None,
            None,
            None,
        )
        .unwrap();
        register(&forged, "bare", &dir, None);

        let swept = sweep_op(&forged, json!({})).unwrap();
        let names: Vec<&str> = swept["reports"]
            .as_array()
            .unwrap()
            .iter()
            .map(|report| report["collector"].as_str().unwrap())
            .collect();
        assert!(names.contains(&"forge-issues"), "{names:?}");
        assert!(names.contains(&"forge-prs"), "{names:?}");
        for read in [
            "forge-checks",
            "forge-labels",
            "forge-releases",
            "forge-posture",
            "forge-notifications",
        ] {
            assert!(names.contains(&read), "{read} is registered: {names:?}");
        }
        for report in swept["reports"].as_array().unwrap() {
            if matches!(
                report["collector"].as_str(),
                Some("forge-issues" | "forge-prs")
            ) {
                assert_eq!(report["outcome"], "ok", "{report}");
                assert_eq!(report["failures"], json!({}), "{report}");
            }
        }

        // Both reach the network, so an offline sweep drops them.
        let offline = sweep_op(&forged, json!({"offline_only": true})).unwrap();
        let offline_names: Vec<&str> = offline["reports"]
            .as_array()
            .unwrap()
            .iter()
            .map(|report| report["collector"].as_str().unwrap())
            .collect();
        assert!(
            !offline_names.contains(&"forge-issues"),
            "{offline_names:?}"
        );
        assert!(!offline_names.contains(&"forge-prs"), "{offline_names:?}");
        for read in [
            "forge-checks",
            "forge-labels",
            "forge-releases",
            "forge-posture",
            "forge-notifications",
        ] {
            assert!(
                !offline_names.contains(&read),
                "{read} needs the network: {offline_names:?}"
            );
        }
    }

    /// The announce runs after a checks collector, and its failure is contained:
    /// the sweep reports the checks collector ok and does not fail, as Python's
    /// does (`collect.py`). The seam makes the announce fail; the sweep is the
    /// thing under test.
    #[test]
    fn a_failed_announce_does_not_fail_the_sweep() {
        use crate::application::ci_watch::seam;
        let dir = std::env::temp_dir().join(format!(
            "vogt-collect-announce-{}-{}",
            std::process::id(),
            unique()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let token = dir.join("forge-token");
        std::fs::write(&token, "secret-token\n").unwrap();
        let mut clock = Some(StepClock::new(moment()));
        let mut ids = Some(SequentialIds::new(None).unwrap());
        crate::application::instance::init(&dir, &mut clock, &mut ids).unwrap();
        let forged = build_context(
            VogtConfig {
                data_dir: dir.clone(),
                github_token_file: Some(token),
                ..VogtConfig::default()
            },
            Some(Principal::new("local:test-user", ActorKind::Human, "Test").unwrap()),
            clock,
            ids,
            None,
            None,
            None,
            None,
        )
        .unwrap();
        // No repository, so forge-checks answers with its unsupported receipt
        // and needs no network: the sweep is offline, and reports the checks.
        register(&forged, "bare", &dir, None);

        seam::ATTEMPTS.with(|attempts| attempts.set(0));
        seam::FAIL.with(|fail| fail.set(true));
        let swept = sweep_op(&forged, json!({"collectors": ["forge-checks"]}));
        seam::FAIL.with(|fail| fail.set(false));

        let swept = swept.expect("a failed announce does not fail the sweep");
        assert_eq!(
            seam::ATTEMPTS.with(std::cell::Cell::get),
            1,
            "announced once"
        );
        let reports = swept["reports"].as_array().unwrap();
        let checks = reports
            .iter()
            .find(|report| report["collector"] == "forge-checks")
            .unwrap_or_else(|| panic!("forge-checks reported: {swept}"));
        assert_eq!(checks["outcome"], "ok", "{checks}");
    }

    /// A configured lane is swept with the rest: its collector runs over the
    /// lane's project and reports one observation, with no receipt, no version
    /// endpoint and no forge to read.
    #[test]
    fn a_configured_deploy_lane_is_swept_and_reports_its_lane() {
        let dir = std::env::temp_dir().join(format!(
            "vogt-collect-lane-{}-{}",
            std::process::id(),
            unique()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let config = VogtConfig {
            data_dir: dir.clone(),
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
        let mut clock = Some(StepClock::new(moment()));
        let mut ids = Some(SequentialIds::new(None).unwrap());
        crate::application::instance::init(&dir, &mut clock, &mut ids).unwrap();
        let laned = build_context(
            config,
            Some(Principal::new("local:test-user", ActorKind::Human, "Test").unwrap()),
            clock,
            ids,
            None,
            None,
            None,
            None,
        )
        .unwrap();
        register(&laned, "alpha", &dir, None);

        let swept = sweep_op(&laned, json!({"collectors": ["deploy-lanes"]})).unwrap();
        let report = &swept["reports"].as_array().unwrap()[0];
        assert_eq!(report["collector"], "deploy-lanes", "{swept}");
        assert_eq!(report["outcome"], "ok", "{swept}");
        assert_eq!(report["new"], 1, "{swept}");
    }

    /// Coverage lists the unported collectors as `never_run` while a forge or a
    /// deploy lane is configured. Omitting them reads as "not configured", which
    /// is what Python says only when neither is.
    #[test]
    fn coverage_lists_unported_collectors_as_never_run() {
        let dir = std::env::temp_dir().join(format!(
            "vogt-coverage-names-{}-{}",
            std::process::id(),
            unique()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let mut clock = Some(StepClock::new(moment()));
        let mut ids = Some(SequentialIds::new(None).unwrap());
        crate::application::instance::init(&dir, &mut clock, &mut ids).unwrap();
        let token = dir.join("forge-token");
        std::fs::write(&token, "secret-token\n").unwrap();

        let forged = build_context(
            VogtConfig {
                data_dir: dir.clone(),
                github_token_file: Some(token),
                ..VogtConfig::default()
            },
            Some(Principal::new("local:test-user", ActorKind::Human, "Test").unwrap()),
            Some(StepClock::new(moment())),
            Some(SequentialIds::new(None).unwrap()),
            None,
            None,
            None,
            None,
        )
        .unwrap();
        let answer = crate::application::coverage::coverage_op(&forged, json!({})).unwrap();
        let names: Vec<&str> = answer["collectors"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| entry["collector"].as_str().unwrap())
            .collect();
        for name in [
            "forge-checks",
            "forge-issues",
            "forge-labels",
            "forge-notifications",
            "forge-posture",
            "forge-prs",
            "forge-releases",
        ] {
            let entry = answer["collectors"]
                .as_array()
                .unwrap()
                .iter()
                .find(|entry| entry["collector"] == name)
                .unwrap_or_else(|| panic!("{name} missing from {names:?}"));
            assert_eq!(entry["status"], "never_run", "{name}");
        }
        assert!(
            !names.contains(&"deploy-lanes"),
            "no lane is configured: {names:?}"
        );

        let laned = build_context(
            VogtConfig {
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
            },
            Some(Principal::new("local:test-user", ActorKind::Human, "Test").unwrap()),
            Some(StepClock::new(moment())),
            Some(SequentialIds::new(None).unwrap()),
            None,
            None,
            None,
            None,
        )
        .unwrap();
        let answer = crate::application::coverage::coverage_op(&laned, json!({})).unwrap();
        let entry = answer["collectors"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["collector"] == "deploy-lanes")
            .expect("deploy-lanes missing");
        assert_eq!(entry["status"], "never_run");
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
