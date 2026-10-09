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
