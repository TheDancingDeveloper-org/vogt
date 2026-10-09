//! Raw evidence and its retention.
//!
//! Ports `observations` from `services/collect.py` and `prune` from
//! `services/retention.py`. The list returns subjects that ranked views filter
//! out, because a suppression hides evidence from views and does not delete it.
//! Retention prunes history, never the newest observation of a subject and
//! never a row a drift proposal still has to be able to point at.

use serde_json::{json, Value};

use crate::application::context::{write_of, AppContext, Built};
use crate::application::resolve;
use crate::application::writes::audited_action;
use crate::core::{Clock, IdFactory};
use crate::errors::VogtError;
use crate::storage::interface::{DeclaredStore, ObservedStore, ReadView};

const NO_EVIDENCE: &str = "no sweep has run; there is no evidence store to read yet";
const PRUNED_EVENT: &str = "observations.pruned";

/// Raw evidence, a page at a time.
///
/// `total` is how many rows this page holds, not how many exist: the store is
/// queried a page at a time and no count is taken behind it, so a page that
/// comes back full means there may be more.
pub fn observations_list_op(ctx: &Built, params: Value) -> Result<Value, VogtError> {
    crate::with_ctx!(ctx, |ctx| list(ctx, &params))
}

fn list<C: Clock, I: IdFactory>(
    ctx: &AppContext<C, I>,
    params: &Value,
) -> Result<Value, VogtError> {
    if !ctx.observed.has_evidence_tables()? {
        return Ok(json!({ "observations": [], "total": 0, "detail": NO_EVIDENCE }));
    }
    let project_id = match params.get("project").and_then(Value::as_str) {
        Some(slug) => Some(resolve::project(&ctx.declared.read()?, slug)?.id),
        None => None,
    };
    let kind = params.get("kind").and_then(Value::as_str);
    let limit = params
        .get("limit")
        .and_then(Value::as_i64)
        .ok_or_else(|| VogtError::InvalidRequest("observations.list needs a limit".to_string()))?;
    let latest_only = params.get("latest_only").and_then(Value::as_bool) == Some(true);
    let found = if latest_only {
        let kinds = kind.map(|one| vec![one.to_string()]).unwrap_or_default();
        ctx.observed.latest(
            &kinds,
            project_id.as_deref(),
            params.get("promoted_only").and_then(Value::as_bool) == Some(true),
            false,
            limit,
        )?
    } else {
        ctx.observed.list_observations(
            kind,
            project_id.as_deref(),
            params.get("subject_key").and_then(Value::as_str),
            limit,
            params
                .get("offset")
                .and_then(Value::as_i64)
                .ok_or_else(|| {
                    VogtError::InvalidRequest("observations.list needs an offset".to_string())
                })?,
        )?
    };
    Ok(json!({ "observations": found, "total": found.len(), "detail": Value::Null }))
}

const KIND_DEP_SCAN: &str = "dep_scan";
const KIND_MIRRORED_SOURCE: &str = "mirrored_source";
const NOT_COLLECTED: &str = "no sweep has run; dependency references are not collected";

/// The dependency graph around one project. Ports `deps`.
///
/// An empty graph is three different answers — nothing references out, the
/// manifests are in a format `dep-refs` does not parse, or nothing has walked
/// the project — and `status` plus `detail` say which one it is.
pub fn deps_op(ctx: &Built, params: Value) -> Result<Value, VogtError> {
    crate::with_ctx!(ctx, |ctx| deps(ctx, &params))
}

fn deps<C: Clock, I: IdFactory>(
    ctx: &AppContext<C, I>,
    params: &Value,
) -> Result<Value, VogtError> {
    let slug = params
        .get("project")
        .and_then(Value::as_str)
        .ok_or_else(|| VogtError::InvalidRequest("deps needs a project".to_string()))?;
    let freshness = crate::application::services::freshness::freshness_of(
        &ctx.observed,
        crate::application::services::now_of(&ctx.clock),
    )?;
    if !ctx.observed.has_evidence_tables()? {
        return Ok(json!({
            "project": slug,
            "references_out": [],
            "referenced_by": [],
            "unresolved": 0,
            "mirrors": [],
            "mirrored_by": [],
            "status": "not_collected",
            "manifests_read": 0,
            "unsupported_manifests": [],
            "unreadable_manifests": [],
            "detail": NOT_COLLECTED,
            "freshness": freshness,
        }));
    }
    let view = ctx.declared.read()?;
    let project = resolve::project(&view, slug)?;
    let out = ctx.observed.dep_refs(Some(&project.id), None)?;
    let incoming = ctx.observed.dep_refs(None, Some(&project.id))?;
    let (mirrors, mirrored_by) = mirrors_of(ctx, &view, &project.id)?;
    let scan = scan_of(ctx, &project.id)?;
    let references = out.len();
    let unresolved = out.iter().filter(|row| row.to_project_id.is_none()).count();
    Ok(json!({
        "project": project.slug,
        "references_out": named(&view, &out)?,
        "referenced_by": named(&view, &incoming)?,
        "unresolved": unresolved,
        "mirrors": mirrors,
        "mirrored_by": mirrored_by,
        "status": if scan.is_some() { "collected" } else { "not_collected" },
        "manifests_read": scan.as_ref().map_or(0, |scan| scan.manifests_read),
        "unsupported_manifests": scan.as_ref().map_or_else(Vec::new, |scan| scan.unsupported.clone()),
        "unreadable_manifests": scan.as_ref().map_or_else(Vec::new, |scan| scan.unreadable.clone()),
        "detail": deps_detail(scan.as_ref(), references),
        "freshness": freshness,
    }))
}

struct ScanRecord {
    manifests_read: i64,
    unsupported: Vec<String>,
    unreadable: Vec<String>,
}

/// The project's newest `dep_scan`, or `None` where nothing has walked it.
fn scan_of<C: Clock, I: IdFactory>(
    ctx: &AppContext<C, I>,
    project_id: &str,
) -> Result<Option<ScanRecord>, VogtError> {
    let seen = ctx.observed.latest(
        &[KIND_DEP_SCAN.to_string()],
        Some(project_id),
        false,
        false,
        1,
    )?;
    let Some(observation) = seen.first() else {
        return Ok(None);
    };
    let payload = &observation.payload;
    Ok(Some(ScanRecord {
        manifests_read: count_of(payload.get("manifests_read")),
        unsupported: strings_of(payload.get("unsupported_manifests")),
        unreadable: strings_of(payload.get("unreadable_manifests")),
    }))
}

/// A payload number, read as one only when it is one. A bool is not one.
fn count_of(value: Option<&Value>) -> i64 {
    match value {
        Some(Value::Number(number)) => number.as_i64().unwrap_or(0),
        _ => 0,
    }
}

fn strings_of(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .map(|item| item.to_string().trim_matches('"').to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// Which zero this is, and nothing when it is not one.
fn deps_detail(scan: Option<&ScanRecord>, references: usize) -> Value {
    let Some(scan) = scan else {
        return json!(
            "`dep-refs` has never walked this project, so these counts are \
             'not collected' rather than 'nothing to find' — run `sweep`"
        );
    };
    if references > 0 {
        return Value::Null;
    }
    if !scan.unsupported.is_empty() {
        let shown = scan
            .unsupported
            .iter()
            .take(5)
            .cloned()
            .collect::<Vec<_>>()
            .join(", ");
        return json!(format!(
            "no references found, and {} manifest(s) are in a format `dep-refs` does not read \
             ({shown}): this zero is the collector's reach, not the project's graph",
            scan.unsupported.len()
        ));
    }
    if !scan.unreadable.is_empty() {
        let shown = scan
            .unreadable
            .iter()
            .take(5)
            .cloned()
            .collect::<Vec<_>>()
            .join(", ");
        return json!(format!(
            "no references found, and {} manifest(s) would not parse ({shown})",
            scan.unreadable.len()
        ));
    }
    if scan.manifests_read == 0 {
        return json!("no manifest was found in this project at all");
    }
    Value::Null
}

/// Mirrored-source relations this project is either end of.
fn mirrors_of<C: Clock, I: IdFactory>(
    ctx: &AppContext<C, I>,
    view: &dyn ReadView,
    project_id: &str,
) -> Result<(Vec<Value>, Vec<Value>), VogtError> {
    let slugs: std::collections::BTreeMap<String, String> = view
        .list_projects(10_000, 0)?
        .into_iter()
        .map(|project| (project.id, project.slug))
        .collect();
    let mut mirrors = Vec::new();
    let mut mirrored_by = Vec::new();
    for observation in ctx.observed.latest(
        &[KIND_MIRRORED_SOURCE.to_string()],
        None,
        false,
        false,
        10_000,
    )? {
        let payload = &observation.payload;
        let Some(carrier) = observation.project_id.as_deref() else {
            continue;
        };
        let published_id = payload
            .get("mirrors_project_id")
            .and_then(Value::as_str)
            .unwrap_or("");
        let view_of = json!({
            "package": text_of(payload.get("package")),
            "project": slugs.get(carrier).cloned().unwrap_or_else(|| carrier.to_string()),
            "mirrors": slugs.get(published_id).cloned().unwrap_or_else(|| {
                text_of(payload.get("mirrors_project_slug"))
            }),
            "local_path": text_of(payload.get("local_path")),
            "manifest": optional_text(payload.get("manifest")),
            "local_version": optional_text(payload.get("local_version")),
            "published_version": optional_text(payload.get("published_version")),
            "observed_at": observation.observed_at,
        });
        if carrier == project_id {
            mirrors.push(view_of.clone());
        }
        if published_id == project_id {
            mirrored_by.push(view_of);
        }
    }
    Ok((mirrors, mirrored_by))
}

fn text_of(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(text)) => text.clone(),
        Some(other) => other.to_string(),
        None => String::new(),
    }
}

fn optional_text(value: Option<&Value>) -> Value {
    match value {
        None | Some(Value::Null) => Value::Null,
        Some(Value::String(text)) => json!(text),
        Some(other) => json!(other.to_string()),
    }
}

/// Attach slugs, so a reference reads without a second lookup.
fn named(view: &dyn ReadView, refs: &[crate::core::DepRef]) -> Result<Vec<Value>, VogtError> {
    let mut named = Vec::with_capacity(refs.len());
    for row in refs {
        let mut value = serde_json::to_value(row).map_err(|err| {
            VogtError::InvalidRequest(format!("a dependency reference will not serialise: {err}"))
        })?;
        let from_slug = view
            .project_by_id(&row.from_project_id)?
            .map(|project| project.slug);
        let to_slug = row
            .to_project_id
            .as_deref()
            .and_then(|id| view.project_by_id(id).ok().flatten())
            .map(|project| project.slug);
        if let Some(object) = value.as_object_mut() {
            object.insert("from_project_slug".to_string(), json!(from_slug));
            object.insert("to_project_slug".to_string(), json!(to_slug));
        }
        named.push(value);
    }
    Ok(named)
}

/// Apply the retention policy to observation history.
///
/// The newest observation per subject is kept indefinitely, and so is anything
/// a drift proposal references, including a resolved one: an accepted change
/// whose evidence has vanished is indistinguishable from an unexplained one.
pub fn observations_prune_op(ctx: &Built, params: Value) -> Result<Value, VogtError> {
    let reason = params
        .get("reason")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            VogtError::InvalidRequest("observations.prune needs a reason".to_string())
        })?;
    crate::with_ctx!(ctx, |ctx| prune(ctx, reason))
}

fn prune<C: Clock + 'static, I: IdFactory + 'static>(
    ctx: &AppContext<C, I>,
    reason: &str,
) -> Result<Value, VogtError> {
    if !ctx.observed.has_evidence_tables()? {
        return Err(VogtError::InvalidRequest(
            "nothing has been swept yet; there is no history to prune".to_string(),
        ));
    }
    let now = clock_now(&ctx.clock);
    let horizon_days = ctx.config.retention_days;
    let horizon = crate::core::Moment::from_unix(now.unix_seconds() - horizon_days * 86_400, 0);
    let protected = ctx.declared.read()?.drift_evidence_ids()?;
    let report = ctx.observed.prune(horizon, &protected)?;
    // Allows get the ordinary horizon; denies, the security-interesting rows,
    // are kept four times longer.
    ctx.declared.prune_auth_decisions(
        horizon,
        crate::core::Moment::from_unix(now.unix_seconds() - horizon_days * 4 * 86_400, 0),
    )?;
    let outcome = json!({
        "removed": report.removed,
        "kept_latest": report.kept_latest,
        "kept_referenced": report.kept_referenced,
        "horizon_days": horizon_days,
    });
    let mut write = write_of(ctx);
    audited_action(
        &mut write,
        "observations.prune",
        reason,
        "instance",
        "observations",
        &outcome,
        PRUNED_EVENT,
        None,
    )?;
    ctx.observed.rebuild_latest()?;
    Ok(outcome)
}

fn clock_now<C: Clock>(clock: &std::sync::Arc<std::sync::Mutex<C>>) -> crate::core::Moment {
    clock
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .now()
}
