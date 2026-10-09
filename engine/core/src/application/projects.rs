//! Projects: registration, scaffolding, the per-repo brief, lifecycle.
//! Ports `src/vogt/application/services/projects.py`.
//!
//! `project.import` is not exposed: the clone it performs is a later service.
//! Ranked backlog is not ported, so the brief says `not_collected` for the
//! counts that come out of the gather rather than inventing a zero.

use std::path::{Component, Path, PathBuf};

use serde::Deserialize;
use serde_json::{json, Value};

use crate::application::context::{AppContext, Built};
use crate::application::resolve;
use crate::application::services::{context_parts, dispatch, next_id, now_of, write_context};
use crate::application::writes::{audited_write, WriteContext, WriteOutcome};
use crate::core::{
    py_repr, slugify, Clock, ComplianceStatus, IdFactory, LinkState, Project, ProjectLifecycle,
    TrustState, LOCAL_SCHEME,
};
use crate::decisions::{default_scaffold, roll_up, Scaffold, NOT_APPLICABLE};
use crate::errors::VogtError;
use crate::storage::interface::{
    DeclaredStore, ObservedStore, ProjectUpdate, ReadView, WorkFilter, WriteTxn,
};
use crate::storage::sqlite::declared::SqliteDeclaredStore;

/// `models.py` `DEFAULT_EXCLUSIONS`.
const DEFAULT_EXCLUSIONS: &[&str] = &[
    ".venv/",
    "node_modules/",
    "target/",
    "dist/",
    "build/",
    ".git/",
    ".claude/",
];

const PROJECT_REGISTER: &str = "project.register";
const PROJECT_TRANSITION: &str = "project.transition";
const PROJECT_UPDATE: &str = "project.update";
const PROJECT_SCAFFOLD: &str = "project.scaffold";

const PROJECT_REGISTERED_EVENT: &str = "project.registered";
const PROJECT_TRANSITIONED_EVENT: &str = "project.transitioned";
const PROJECT_UPDATED_EVENT: &str = "project.updated";
const PROJECT_SCAFFOLDED_EVENT: &str = "project.scaffolded";

/// `dep_refs.py` `KIND_DEP_SCAN`. The collector itself is not ported; the kind
/// string is what the observed store is queried by.
const KIND_DEP_SCAN: &str = "dep_scan";

/// Ranked backlog is `views._gather`, which is not ported.
const BACKLOG_NOT_COLLECTED: &str = "ranked backlog is not ported yet";

// --- parameters -------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RegisterProjectParams {
    name: String,
    root_path: String,
    #[serde(default)]
    repo_url: Option<String>,
    #[serde(default = "default_active")]
    lifecycle_state: String,
    #[serde(default)]
    exclusions: Option<Vec<String>>,
    reason: String,
}

fn default_active() -> String {
    "active".to_string()
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateProjectParams {
    name: String,
    root_path: String,
    #[serde(default)]
    owner: Option<String>,
    #[serde(default)]
    repo_url: Option<String>,
    #[serde(default = "default_incubating")]
    lifecycle_state: String,
    reason: String,
}

fn default_incubating() -> String {
    "incubating".to_string()
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateProjectParams {
    slug: String,
    #[serde(default)]
    repo_url: Option<String>,
    #[serde(default)]
    exclusions: Option<Vec<String>>,
    reason: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TransitionProjectParams {
    slug: String,
    to_state: String,
    reason: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GetProjectParams {
    slug: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ListProjectsParams {
    #[serde(default = "default_limit")]
    limit: i64,
    #[serde(default)]
    offset: i64,
}

fn default_limit() -> i64 {
    50
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProjectBriefParams {
    slug: String,
    #[serde(default = "default_backlog_limit")]
    backlog_limit: i64,
    #[serde(default = "default_mode")]
    mode: String,
}

fn default_backlog_limit() -> i64 {
    10
}

fn default_mode() -> String {
    "summary".to_string()
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ScaffoldProjectParams {
    project: String,
    reason: String,
}

fn parse<T: for<'de> Deserialize<'de>>(params: Value) -> Result<T, VogtError> {
    serde_json::from_value(params).map_err(|err| VogtError::InvalidRequest(err.to_string()))
}

/// Pydantic aliases `project` and `id` onto `slug` before `extra=forbid` runs,
/// so a brief that names the project that way is not an unknown field.
fn brief_params(mut params: Value) -> Result<ProjectBriefParams, VogtError> {
    if let Value::Object(map) = &mut params {
        if !map.contains_key("slug") {
            if let Some(alias) = map.remove("project").or_else(|| map.remove("id")) {
                map.insert("slug".to_string(), alias);
            }
        }
        map.remove("project");
        map.remove("id");
    }
    let parsed = parse::<ProjectBriefParams>(params)?;
    if !(1..=100).contains(&parsed.backlog_limit) {
        return Err(VogtError::InvalidRequest(
            "backlog_limit must be between 1 and 100".to_string(),
        ));
    }
    Ok(parsed)
}

fn list_params(params: Value) -> Result<ListProjectsParams, VogtError> {
    let parsed = parse::<ListProjectsParams>(params)?;
    if !(1..=500).contains(&parsed.limit) {
        return Err(VogtError::InvalidRequest(
            "limit must be between 1 and 500".to_string(),
        ));
    }
    if parsed.offset < 0 {
        return Err(VogtError::InvalidRequest(
            "offset must be 0 or greater".to_string(),
        ));
    }
    Ok(parsed)
}

fn parse_lifecycle(value: &str) -> Result<ProjectLifecycle, VogtError> {
    value.parse().map_err(|_| {
        VogtError::InvalidRequest(format!("unknown lifecycle state {}", py_repr(value)))
    })
}

// --- the operation arms -----------------------------------------------------

pub fn register_op(ctx: &Built, params: Value) -> Result<Value, VogtError> {
    dispatch!(ctx, register_project, params)
}
pub fn create_op(ctx: &Built, params: Value) -> Result<Value, VogtError> {
    dispatch!(ctx, create_project, params)
}
pub fn scaffold_op(ctx: &Built, params: Value) -> Result<Value, VogtError> {
    dispatch!(ctx, scaffold_project, params)
}
pub fn update_op(ctx: &Built, params: Value) -> Result<Value, VogtError> {
    dispatch!(ctx, update_project, params)
}
pub fn transition_op(ctx: &Built, params: Value) -> Result<Value, VogtError> {
    dispatch!(ctx, transition_project, params)
}
pub fn get_op(ctx: &Built, params: Value) -> Result<Value, VogtError> {
    dispatch!(ctx, get_project, params)
}
pub fn list_op(ctx: &Built, params: Value) -> Result<Value, VogtError> {
    dispatch!(ctx, list_projects, params)
}
pub fn brief_op(ctx: &Built, params: Value) -> Result<Value, VogtError> {
    dispatch!(ctx, brief_project, params)
}

// --- registration -----------------------------------------------------------

/// `pathlib.Path.expanduser`, without `resolve`. `~` is the current user's
/// home; `~other` is left as written, matching Python when the name is unknown.
fn expand_user(path: &Path) -> PathBuf {
    let mut components = path.components();
    let Some(Component::Normal(first)) = components.next() else {
        return path.to_path_buf();
    };
    let text = first.to_string_lossy();
    if text == "~" {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        return match home {
            Some(home) => home.join(components.as_path()),
            None => path.to_path_buf(),
        };
    }
    path.to_path_buf()
}

/// `Path.expanduser().resolve()`, which canonicalises what exists and
/// normalises the rest.
fn resolve_path(path: &Path) -> PathBuf {
    let expanded = expand_user(path);
    std::fs::canonicalize(&expanded).unwrap_or(expanded)
}

/// Python's `relative_to`: `path` is inside `root`, or is `root`.
fn within(path: &Path, root: &Path) -> bool {
    let path = resolve_path(path);
    let root = resolve_path(root);
    path.starts_with(&root)
}

/// A remote caller may only name a root inside the configured import root. A
/// `local:` principal already owns the filesystem, so the check is skipped.
fn guard_remote_root_path(
    identity_ref: &str,
    import_root: &Path,
    root_path: &str,
) -> Result<(), VogtError> {
    let prefix = format!("{LOCAL_SCHEME}:");
    if identity_ref.starts_with(&prefix) {
        return Ok(());
    }
    if !within(Path::new(root_path), import_root) {
        return Err(VogtError::InvalidRequest(format!(
            "root_path must be within the configured import root ({}); a remote caller may not name an arbitrary server filesystem path",
            import_root.display()
        )));
    }
    Ok(())
}

fn slug_for(name: &str) -> Result<String, VogtError> {
    let slug = slugify(name);
    if slug.is_empty() {
        return Err(VogtError::InvalidRequest(format!(
            "cannot derive a slug from name {}",
            py_repr(name)
        )));
    }
    Ok(slug)
}

/// The declared half of registration. `create` calls it with
/// `adopts_contract`; `register` does not. The audit operation stays
/// `project.register` for both, which is what Python records.
struct Registration {
    operation: &'static str,
    event_kind: &'static str,
    name: String,
    root_path: String,
    repo_url: Option<String>,
    lifecycle_state: String,
    exclusions: Option<Vec<String>>,
    adopts_contract: bool,
    link_state: LinkState,
    now: crate::core::Moment,
    project_id: String,
}

fn record_registration<C: Clock + 'static, I: IdFactory + 'static>(
    writing: &mut WriteContext<'_, C, I, SqliteDeclaredStore<C, I>>,
    reason: &str,
    input: Registration,
) -> Result<Project, VogtError> {
    let slug = slug_for(&input.name)?;
    let event = input.event_kind.to_string();
    audited_write(writing, input.operation, reason, move |txn, _actor| {
        if txn.project_by_slug(&slug)?.is_some() {
            return Err(VogtError::Conflict(format!(
                "a project with slug {} is already registered",
                py_repr(&slug)
            )));
        }
        let project = Project {
            id: input.project_id,
            slug: slug.clone(),
            name: input.name.clone(),
            root_path: input.root_path,
            repo_url: input.repo_url,
            lifecycle_state: parse_lifecycle(&input.lifecycle_state)?,
            current_version: None,
            contract_version: None,
            compliance_status: ComplianceStatus::NotChecked,
            compliance_checked_at: None,
            contract_adopted_at: if input.adopts_contract {
                Some(input.now)
            } else {
                None
            },
            write_back: crate::core::WriteBack::Disabled,
            link_state: input.link_state,
            exclusions: match input.exclusions {
                None => DEFAULT_EXCLUSIONS
                    .iter()
                    .map(|entry| (*entry).to_string())
                    .collect(),
                Some(given) => given,
            },
            trust_state: TrustState::Unverified,
            created_at: input.now,
            updated_at: input.now,
        };
        txn.insert_project(&project)?;
        let payload = serde_json::to_value(&project).unwrap_or(Value::Null);
        let id = project.id.clone();
        Ok(WriteOutcome::new(
            project,
            "project",
            &id,
            payload,
            &event,
            json!({"slug": slug, "name": input.name}),
        ))
    })
}

fn register_project<C: Clock + 'static, I: IdFactory + 'static>(
    ctx: &AppContext<C, I>,
    params: Value,
) -> Result<Value, VogtError> {
    let params = parse::<RegisterProjectParams>(params)?;
    let (declared, _, principal, clock, ids, config) = context_parts(ctx);
    guard_remote_root_path(
        &principal.identity_ref,
        &config.resolved_import_root(),
        &params.root_path,
    )?;
    let now = now_of(&clock);
    let project_id = next_id(&ids, "prj");
    let mut writing = write_context(declared, principal, clock, ids);
    let project = record_registration(
        &mut writing,
        &params.reason,
        Registration {
            operation: PROJECT_REGISTER,
            event_kind: PROJECT_REGISTERED_EVENT,
            name: params.name,
            root_path: params.root_path,
            repo_url: params.repo_url,
            lifecycle_state: params.lifecycle_state,
            exclusions: params.exclusions,
            adopts_contract: false,
            link_state: LinkState::Unlinked,
            now,
            project_id,
        },
    )?;
    Ok(json!({"project": project}))
}

// --- scaffold ---------------------------------------------------------------

/// Write a scaffold, never overwriting. An existing path is skipped.
fn lay_scaffold(
    root: &Path,
    scaffold: &Scaffold,
    create_root: bool,
) -> Result<(Vec<String>, Vec<String>), VogtError> {
    let mut created = Vec::new();
    let mut skipped = Vec::new();
    if !root.exists() {
        if !create_root {
            return Err(VogtError::NotFound(format!(
                "{} does not exist, so there is nothing to scaffold into",
                root.display()
            )));
        }
        std::fs::create_dir_all(root).map_err(|err| {
            VogtError::InvalidRequest(format!("could not create {}: {err}", root.display()))
        })?;
        created.push(root.display().to_string());
    }
    for directory in &scaffold.directories {
        let target = root.join(directory);
        if target.exists() {
            skipped.push(target.display().to_string());
        } else {
            std::fs::create_dir_all(&target).map_err(|err| {
                VogtError::InvalidRequest(format!("could not create {}: {err}", target.display()))
            })?;
            created.push(target.display().to_string());
        }
    }
    for entry in &scaffold.files {
        let target = root.join(&entry.path);
        if target.exists() {
            skipped.push(target.display().to_string());
        } else {
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent).map_err(|err| {
                    VogtError::InvalidRequest(format!(
                        "could not create {}: {err}",
                        parent.display()
                    ))
                })?;
            }
            std::fs::write(&target, entry.content.as_bytes()).map_err(|err| {
                VogtError::InvalidRequest(format!("could not write {}: {err}", target.display()))
            })?;
            created.push(target.display().to_string());
        }
    }
    Ok((created, skipped))
}

fn record_scaffold<C: Clock + 'static, I: IdFactory + 'static>(
    writing: &mut WriteContext<'_, C, I, SqliteDeclaredStore<C, I>>,
    reason: &str,
    project_id: String,
    project_slug: String,
    created: Vec<String>,
    skipped: Vec<String>,
) -> Result<(), VogtError> {
    let created_count = created.len();
    let skipped_count = skipped.len();
    audited_write(writing, PROJECT_SCAFFOLD, reason, move |_txn, _actor| {
        Ok(WriteOutcome::new(
            (),
            "project",
            &project_id,
            json!({"created": created, "skipped": skipped}),
            PROJECT_SCAFFOLDED_EVENT,
            json!({"slug": project_slug, "created": created_count, "skipped": skipped_count}),
        ))
    })
}

fn scaffold_project<C: Clock + 'static, I: IdFactory + 'static>(
    ctx: &AppContext<C, I>,
    params: Value,
) -> Result<Value, VogtError> {
    let params = parse::<ScaffoldProjectParams>(params)?;
    let project = resolve::project(&ctx.declared.read()?, &params.project)?;
    let root = expand_user(Path::new(&project.root_path));
    let scaffold = default_scaffold(
        &project.name,
        &ctx.principal.display_name,
        &project.lifecycle_state.to_string(),
    );
    let (created, skipped) = lay_scaffold(&root, &scaffold, false)?;
    let created_sorted = sorted(&created);
    let skipped_sorted = sorted(&skipped);
    let result = json!({
        "project": project.slug,
        "root_path": root.display().to_string(),
        "created": created_sorted,
        "skipped": skipped_sorted,
        "detail": format!(
            "{} written, {} already there and left exactly as they were",
            created.len(),
            skipped.len()
        ),
    });
    let (declared, _, principal, clock, ids, _) = context_parts(ctx);
    let mut writing = write_context(
        declared,
        principal,
        std::sync::Arc::clone(&clock),
        std::sync::Arc::clone(&ids),
    );
    record_scaffold(
        &mut writing,
        &params.reason,
        project.id,
        project.slug,
        created_sorted,
        skipped_sorted,
    )?;
    Ok(result)
}

fn sorted(paths: &[String]) -> Vec<String> {
    let mut out = paths.to_vec();
    out.sort();
    out
}

fn create_project<C: Clock + 'static, I: IdFactory + 'static>(
    ctx: &AppContext<C, I>,
    params: Value,
) -> Result<Value, VogtError> {
    let params = parse::<CreateProjectParams>(params)?;
    let (declared, _, principal, clock, ids, config) = context_parts(ctx);
    guard_remote_root_path(
        &principal.identity_ref,
        &config.resolved_import_root(),
        &params.root_path,
    )?;
    let owner = params
        .owner
        .clone()
        .unwrap_or_else(|| principal.display_name.clone());
    let root = expand_user(Path::new(&params.root_path));
    let scaffold = default_scaffold(&params.name, &owner, &params.lifecycle_state);
    let (created, skipped) = lay_scaffold(&root, &scaffold, true)?;
    let now = now_of(&clock);
    let project_id = next_id(&ids, "prj");
    let mut writing = write_context(declared, principal, clock, ids);
    let project = record_registration(
        &mut writing,
        &params.reason,
        Registration {
            operation: PROJECT_REGISTER,
            event_kind: PROJECT_REGISTERED_EVENT,
            name: params.name,
            root_path: root.display().to_string(),
            repo_url: params.repo_url,
            lifecycle_state: params.lifecycle_state,
            exclusions: None,
            adopts_contract: true,
            link_state: LinkState::Unlinked,
            now,
            project_id,
        },
    )?;
    let mut result = json!({"project": project});
    if let Value::Object(map) = &mut result {
        map.insert("created_paths".to_string(), json!(sorted(&created)));
        map.insert("skipped_paths".to_string(), json!(sorted(&skipped)));
    }
    Ok(result)
}

// --- reads ------------------------------------------------------------------

fn get_project<C: Clock, I: IdFactory>(
    ctx: &AppContext<C, I>,
    params: Value,
) -> Result<Value, VogtError> {
    let params = parse::<GetProjectParams>(params)?;
    let view = ctx.declared.read()?;
    let project = resolve::project(&view, &params.slug)?;
    Ok(json!({"project": project}))
}

fn list_projects<C: Clock, I: IdFactory>(
    ctx: &AppContext<C, I>,
    params: Value,
) -> Result<Value, VogtError> {
    let params = list_params(params)?;
    let view = ctx.declared.read()?;
    let projects = view.list_projects(params.limit, params.offset)?;
    let total = view.counts()?.projects;
    let listings: Vec<Value> = projects
        .iter()
        .map(|project| {
            let mut listing = serde_json::to_value(project).unwrap_or(Value::Null);
            if let Value::Object(map) = &mut listing {
                let (writable, reason) = writability(project);
                map.insert("writable".to_string(), Value::Bool(writable));
                map.insert("writable_reason".to_string(), Value::String(reason));
            }
            listing
        })
        .collect();
    Ok(json!({"projects": listings, "total": total}))
}

/// Whether `work.create` lands in a project right now. Ports
/// `work.create_writability`.
///
/// An unlinked project is refused outright: there is no forge issue to write
/// through to, so the answer depends only on the link state. A linked one also
/// needs the write-back policy to permit `create`. The credential gate beyond
/// that (`writeback._writer_provider`) is not ported, so a linked project whose
/// policy allows the action is reported with that gap named rather than as a
/// guessed yes.
fn writability(project: &Project) -> (bool, String) {
    if project.link_state == LinkState::Unlinked {
        return (
            false,
            "not forge-linked: work.create refuses with project_not_linked. Pass \
             local_only=true for a local record, or link (`forge link`) or publish \
             (`forge publish`) the project."
                .to_string(),
        );
    }
    let policy = project.write_back.to_string();
    if !crate::adapters::forge::permits(&policy, "create") {
        return (
            false,
            format!(
                "write-back policy is {}, which does not permit 'create'",
                py_repr(&policy)
            ),
        );
    }
    (
        false,
        "forge credential resolution is not ported, so a linked project's write is not confirmed"
            .to_string(),
    )
}

fn brief_project<C: Clock, I: IdFactory>(
    ctx: &AppContext<C, I>,
    params: Value,
) -> Result<Value, VogtError> {
    let params = brief_params(params)?;
    let view = ctx.declared.read()?;
    let project = resolve::project(&view, &params.slug)?;
    let items = view.list_work_items(&WorkFilter {
        project_id: Some(project.id.clone()),
        limit: 1000,
        ..WorkFilter::default()
    })?;
    let mut by_state: Vec<(String, i64)> = Vec::new();
    let mut by_kind: Vec<(String, i64)> = Vec::new();
    for item in &items {
        tally(&mut by_state, &item.state.to_string());
        tally(&mut by_kind, &item.kind.to_string());
    }
    by_state.sort();
    by_kind.sort();
    let observed_version = observed_version(ctx, &project.id)?;
    let version_matches = match (&observed_version, &project.current_version) {
        (Some(observed), Some(declared)) => {
            Some(observed.trim_start_matches('v') == declared.trim_start_matches('v'))
        }
        _ => None,
    };
    let adopted = project.contract_adopted_at.is_some();
    Ok(json!({
        "project": project,
        // The gather that ranks open work is not ported. These are not zeros:
        // a zero would say a sweep found nothing.
        "open_work": "not_collected",
        "open_bugs": "not_collected",
        "declared_work": "not_collected",
        "observed_work": "not_collected",
        "by_state": tally_object(&by_state),
        "by_kind": tally_object(&by_kind),
        "top_backlog": {"status": "not_collected", "detail": BACKLOG_NOT_COLLECTED},
        "current_version": project.current_version,
        "declared_version": project.current_version,
        "observed_version": observed_version,
        "version_matches": version_matches,
        "compliance_status": if adopted {
            project.compliance_status.to_string()
        } else {
            NOT_APPLICABLE.to_string()
        },
        "compliance_checked_at": if adopted { project.compliance_checked_at } else { None },
        "ci_status": ci_summary(ctx, &project.id)?,
        "dependencies": dependency_summary(ctx, &project.id)?,
        "freshness": crate::application::services::freshness::freshness_of(
            &ctx.observed,
            now_of(&ctx.clock),
        )?,
        "backlog_limit": params.backlog_limit,
        "mode": params.mode,
    }))
}

fn tally(into: &mut Vec<(String, i64)>, key: &str) {
    if let Some(found) = into.iter_mut().find(|(name, _)| name == key) {
        found.1 += 1;
    } else {
        into.push((key.to_string(), 1));
    }
}

fn tally_object(pairs: &[(String, i64)]) -> Value {
    let mut map = serde_json::Map::new();
    for (key, count) in pairs {
        map.insert(key.clone(), json!(count));
    }
    Value::Object(map)
}

fn observed_version<C: Clock, I: IdFactory>(
    ctx: &AppContext<C, I>,
    project_id: &str,
) -> Result<Option<String>, VogtError> {
    if !ctx.observed.has_evidence_tables()? {
        return Ok(None);
    }
    let seen = ctx.observed.latest(
        &["git.tag".to_string(), "release".to_string()],
        Some(project_id),
        false,
        false,
        100,
    )?;
    let mut tags: Vec<String> = seen
        .iter()
        .filter_map(|observation| match observation.payload.get("tag") {
            Some(Value::String(tag)) if !tag.is_empty() => Some(tag.clone()),
            _ => None,
        })
        .collect();
    tags.sort();
    Ok(tags.pop())
}

fn ci_summary<C: Clock, I: IdFactory>(
    ctx: &AppContext<C, I>,
    project_id: &str,
) -> Result<Value, VogtError> {
    if !ctx.observed.has_evidence_tables()? {
        // `status` keeps its model default and is omitted: Python's
        // `exclude_defaults` drops it, so emitting it here would be a key the
        // other side never returns.
        return Ok(json!({
            "checks": 0,
            "failing": [],
            "revision": Value::Null,
            "revisions_observed": 0,
            "earlier_failures": 0,
            "detail": "no sweep has run; CI status is not collected",
        }));
    }
    let checks = ctx.observed.latest(
        &["ci.check".to_string()],
        Some(project_id),
        false,
        false,
        200,
    )?;
    let Some(rollup) = roll_up(&checks) else {
        return Ok(json!({
            "status": "no_checks",
            "checks": 0,
            "failing": [],
            "revision": Value::Null,
            "revisions_observed": 0,
            "earlier_failures": 0,
            "detail": "swept, but no CI checks were observed — either this project has none, or the optional forge adapter is not configured",
        }));
    };
    let detail = if rollup.earlier_failures == 0 {
        Value::Null
    } else {
        json!(format!(
            "{} failing check(s) on earlier revisions are not counted here; this is the state of {} alone",
            rollup.earlier_failures,
            &rollup.revision[..rollup.revision.len().min(12)]
        ))
    };
    Ok(json!({
        "status": rollup.status(),
        "checks": rollup.checks.len(),
        "failing": rollup.failing,
        "revision": rollup.revision,
        "revisions_observed": rollup.revisions_observed,
        "earlier_failures": rollup.earlier_failures,
        "detail": detail,
    }))
}

fn dependency_summary<C: Clock, I: IdFactory>(
    ctx: &AppContext<C, I>,
    project_id: &str,
) -> Result<Value, VogtError> {
    if !ctx.observed.has_evidence_tables()? {
        return Ok(json!({
            "status": "not_collected",
            "references_out": 0,
            "referenced_by": 0,
            "unresolved": 0,
            "detail": "no sweep has run; dependency references are not collected",
        }));
    }
    let walked = ctx.observed.latest(
        &[KIND_DEP_SCAN.to_string()],
        Some(project_id),
        false,
        false,
        1,
    )?;
    if walked.is_empty() {
        return Ok(json!({
            "status": "not_collected",
            "references_out": 0,
            "referenced_by": 0,
            "unresolved": 0,
            "detail": "`dep-refs` has not walked this project; its references are not collected, which is not the same as none",
        }));
    }
    let out = ctx.observed.dep_refs(Some(project_id), None)?;
    let incoming = ctx.observed.dep_refs(None, Some(project_id))?;
    let unresolved = out.iter().filter(|row| row.to_project_id.is_none()).count();
    Ok(json!({
        "status": "collected",
        "references_out": out.len(),
        "referenced_by": incoming.len(),
        "unresolved": unresolved,
    }))
}

struct ProjectChange {
    slug: String,
    repo_url: Option<String>,
    exclusions: Option<Vec<String>>,
    lifecycle_state: Option<String>,
    at: crate::core::Moment,
    event: &'static str,
}

fn apply_change<C: Clock + 'static, I: IdFactory + 'static>(
    writing: &mut WriteContext<'_, C, I, SqliteDeclaredStore<C, I>>,
    operation: &str,
    reason: &str,
    change: ProjectChange,
) -> Result<Project, VogtError> {
    audited_write(writing, operation, reason, move |txn, _actor| {
        let project = resolve::project(txn, &change.slug)?;
        if change.event == PROJECT_UPDATED_EVENT
            && change.repo_url.is_none()
            && change.exclusions.is_none()
        {
            return Err(VogtError::InvalidRequest(
                "give --repo-url or --exclusions; there is nothing else to update".to_string(),
            ));
        }
        if change.event == PROJECT_TRANSITIONED_EVENT {
            crate::core::check_lifecycle_transition(
                &project.lifecycle_state.to_string(),
                change.lifecycle_state.as_deref().unwrap_or(""),
            )?;
        }
        txn.update_project(
            &project.id,
            &ProjectUpdate {
                repo_url: change.repo_url.clone(),
                exclusions: change.exclusions.clone(),
                lifecycle_state: change.lifecycle_state.clone(),
                ..ProjectUpdate::default()
            },
            change.at,
        )?;
        let updated = txn.project_by_slug(&project.slug)?.ok_or_else(|| {
            VogtError::NotFound(format!("no project with slug {}", py_repr(&project.slug)))
        })?;
        let payload = serde_json::to_value(&updated).unwrap_or(Value::Null);
        let summary = if change.event == PROJECT_TRANSITIONED_EVENT {
            json!({"slug": project.slug, "from": project.lifecycle_state.to_string(), "to": change.lifecycle_state})
        } else {
            json!({"slug": project.slug, "exclusions": updated.exclusions, "repo_url": updated.repo_url})
        };
        let id = project.id.clone();
        Ok(WriteOutcome::new(
            updated,
            "project",
            &id,
            payload,
            change.event,
            summary,
        ))
    })
}

// --- updates ----------------------------------------------------------------

fn place_change<C: Clock + 'static, I: IdFactory + 'static>(
    ctx: &AppContext<C, I>,
    change: ProjectChange,
    operation: &str,
    reason: &str,
) -> Result<Project, VogtError> {
    let (declared, _, principal, clock, ids, _) = context_parts(ctx);
    let mut writing = write_context(
        declared,
        principal,
        std::sync::Arc::clone(&clock),
        std::sync::Arc::clone(&ids),
    );
    apply_change(&mut writing, operation, reason, change)
}

fn update_project<C: Clock + 'static, I: IdFactory + 'static>(
    ctx: &AppContext<C, I>,
    params: Value,
) -> Result<Value, VogtError> {
    let params = parse::<UpdateProjectParams>(params)?;
    let updated = place_change(
        ctx,
        ProjectChange {
            slug: params.slug,
            repo_url: params.repo_url,
            exclusions: params.exclusions,
            lifecycle_state: None,
            at: now_of(&ctx.clock),
            event: PROJECT_UPDATED_EVENT,
        },
        PROJECT_UPDATE,
        &params.reason,
    )?;
    Ok(json!({"project": updated}))
}

fn transition_project<C: Clock + 'static, I: IdFactory + 'static>(
    ctx: &AppContext<C, I>,
    params: Value,
) -> Result<Value, VogtError> {
    let params = parse::<TransitionProjectParams>(params)?;
    let updated = place_change(
        ctx,
        ProjectChange {
            slug: params.slug,
            repo_url: None,
            exclusions: None,
            lifecycle_state: Some(params.to_state),
            at: now_of(&ctx.clock),
            event: PROJECT_TRANSITIONED_EVENT,
        },
        PROJECT_TRANSITION,
        &params.reason,
    )?;
    Ok(json!({"project": updated}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{ActorKind, Principal, SequentialIds, StepClock};
    use crate::storage::interface::DeclaredStore;

    fn moment() -> crate::core::Moment {
        crate::core::Moment::from_unix(1_700_000_000, 0)
    }

    fn unique() -> u64 {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        COUNTER.fetch_add(1, Ordering::Relaxed)
    }

    /// A migrated, bootstrapped context on a step clock and sequential ids, so a
    /// test draws the same id every time it runs.
    fn context(identity: &str, import_root: Option<PathBuf>) -> Built {
        let dir =
            std::env::temp_dir().join(format!("vogt-projects-{}-{}", std::process::id(), unique()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let config = crate::config::VogtConfig {
            data_dir: dir,
            import_root,
            ..crate::config::VogtConfig::default()
        };
        let principal = Principal::new(identity, ActorKind::Human, "Test").unwrap();
        let built = crate::application::context::build_context(
            config,
            Some(principal.clone()),
            Some(StepClock::new(moment())),
            Some(SequentialIds::new(None).unwrap()),
            None,
            None,
            None,
            None,
        )
        .unwrap();
        let Built::StepSequential(ctx) = &built else {
            unreachable!("the test asks for a step clock and sequential ids");
        };
        ctx.declared.migrate().unwrap();
        ctx.declared.bootstrap(&principal).unwrap();
        built
    }

    fn registration(name: &str, root: &str) -> Value {
        json!({
            "name": name,
            "root_path": root,
            "reason": "a reason",
        })
    }

    #[test]
    fn a_duplicate_slug_is_a_conflict() {
        let ctx = context("local:test-user", None);
        register_op(&ctx, registration("Widget", "/srv/widget")).unwrap();
        let error = register_op(&ctx, registration("Widget", "/srv/other")).unwrap_err();
        assert!(
            matches!(error, VogtError::Conflict(ref message) if message == "a project with slug 'widget' is already registered"),
            "{error}"
        );
    }

    #[test]
    fn a_remote_caller_may_not_name_a_path_outside_the_import_root() {
        let import = std::env::temp_dir().join(format!("vogt-import-{}", unique()));
        std::fs::create_dir_all(&import).unwrap();
        let ctx = context("token:remote", Some(import.clone()));
        let error = register_op(&ctx, registration("Widget", "/etc/passwd")).unwrap_err();
        assert!(
            matches!(error, VogtError::InvalidRequest(ref message) if message.contains("root_path must be within the configured import root")),
            "{error}"
        );

        // A local principal owns the filesystem, so the same path is allowed.
        let local = context("local:test-user", Some(import));
        register_op(&local, registration("Widget", "/etc/passwd")).unwrap();
    }

    #[test]
    fn scaffolding_leaves_an_existing_file_untouched() {
        let root = std::env::temp_dir().join(format!("vogt-scaffold-{}", unique()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("README.md"), "kept").unwrap();
        let ctx = context("local:test-user", None);
        create_op(&ctx, registration("Widget", &root.display().to_string())).unwrap();
        let result = scaffold_op(&ctx, json!({"project": "widget", "reason": "a reason"})).unwrap();
        assert!(result["skipped"].as_array().unwrap().iter().any(|entry| {
            entry
                .as_str()
                .is_some_and(|path| path.ends_with("README.md"))
        }));
        assert_eq!(
            std::fs::read_to_string(root.join("README.md")).unwrap(),
            "kept"
        );
    }

    #[test]
    fn create_adopts_the_contract_and_register_does_not() {
        let ctx = context("local:test-user", None);
        let registered = register_op(&ctx, registration("Registered", "/srv/registered")).unwrap();
        assert!(registered["project"]["contract_adopted_at"].is_null());
        let root = std::env::temp_dir().join(format!("vogt-created-{}", unique()));
        let created =
            create_op(&ctx, registration("Created", &root.display().to_string())).unwrap();
        assert!(created["project"]["contract_adopted_at"].is_string());
    }

    #[test]
    fn an_illegal_lifecycle_transition_is_rejected() {
        let ctx = context("local:test-user", None);
        register_op(&ctx, registration("Widget", "/srv/widget")).unwrap();
        let error = transition_op(
            &ctx,
            json!({"slug": "widget", "to_state": "retired", "reason": "a reason"}),
        )
        .unwrap_err();
        assert!(
            matches!(error, VogtError::TransitionRejected { ref rule, .. } if rule == "lifecycle.not_allowed"),
            "{error}"
        );
    }
}
