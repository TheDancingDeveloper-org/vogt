//! Suppression — the decision that an observed subject is not work.
//!
//! Ports the suppression half of `src/vogt/application/services/observed_first.py`.
//! The subject stays observable and queryable; the decision hides it from
//! views, it does not delete evidence. It survives re-observation because it
//! lives in the declared store.

use serde_json::{json, Value};

use crate::application::context::{write_of, AppContext, Built};
use crate::application::resolve;
use crate::application::writes::{audited_write, WriteOutcome};
use crate::core::{Clock, IdFactory};
use crate::errors::VogtError;
use crate::storage::interface::{DeclaredStore, ReadView, WriteTxn};

const SUPPRESSED_EVENT: &str = "subject.suppressed";
const SUPPRESSION_REVOKED_EVENT: &str = "subject.unsuppressed";

/// Remove an observed subject from ranked views, permanently.
///
/// A first-class operation rather than adopt plus wont_do, because the latter
/// fabricates a declared work item for every piece of noise.
pub fn suppress_op(ctx: &Built, params: Value) -> Result<Value, VogtError> {
    let subject = field(&params, "suppress", "subject")?;
    let reason = field(&params, "suppress", "reason")?;
    let pattern = params
        .get("pattern")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let project = params
        .get("project")
        .and_then(Value::as_str)
        .map(str::to_string);
    crate::with_ctx!(ctx, |ctx| suppress(
        ctx,
        &subject,
        pattern,
        project.as_deref(),
        &reason
    ))
}

fn suppress<C: Clock + 'static, I: IdFactory + 'static>(
    ctx: &AppContext<C, I>,
    subject: &str,
    pattern: bool,
    project: Option<&str>,
    reason: &str,
) -> Result<Value, VogtError> {
    if subject.is_empty() {
        return Err(VogtError::InvalidRequest(
            "suppress needs a non-empty subject".to_string(),
        ));
    }
    let subject = subject.to_string();
    let project = project.map(str::to_string);
    let reason = reason.to_string();
    let mut write = write_of(ctx);
    let ids = std::sync::Arc::clone(write.ids());
    let clock = std::sync::Arc::clone(write.clock());
    let match_kind = if pattern { "pattern" } else { "exact" };
    audited_write(&mut write, "suppress", &reason, |txn, actor| {
        // Resolution and the duplicate check happen before the id and the
        // timestamp are drawn, matching `observed_first.py`: a duplicate must
        // not burn a `sup` id, and an unknown project must still consume the
        // transaction id the write already opened.
        let scope = match project.as_deref() {
            Some(slug) => Some(resolve::project(&*txn, slug)?.id),
            None => None,
        };
        for existing in txn.list_suppressions(false, 1000)? {
            if existing.subject_key_or_pattern == subject && existing.scope_project_id == scope {
                return Err(VogtError::Conflict(format!(
                    "{} is already suppressed ({})",
                    crate::core::py_repr(&subject),
                    existing.id
                )));
            }
        }
        let id = ids
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .next("sup");
        let now = clock_now(&clock);
        let suppression = crate::core::Suppression {
            id: id.clone(),
            match_kind: if pattern {
                crate::core::MatchKind::Pattern
            } else {
                crate::core::MatchKind::Exact
            },
            subject_key_or_pattern: subject.to_string(),
            scope_project_id: scope.clone(),
            scope_project_slug: None,
            actor_id: actor.id.clone(),
            actor_identity_ref: Some(actor.identity_ref.clone()),
            reason: reason.clone(),
            created_at: now,
            revoked_at: None,
            revoked_reason: None,
        };
        txn.insert_suppression(&suppression)?;
        Ok(WriteOutcome {
            result: json!({ "suppression": suppression }),
            entity_kind: "suppression".to_string(),
            entity_id: id.clone(),
            payload: serde_json::to_value(&suppression).unwrap_or(Value::Null),
            event_kind: SUPPRESSED_EVENT.to_string(),
            summary: json!({ "subject": subject, "match_kind": match_kind }),
        })
    })
}

/// The suppressions on record. Revoked ones are history, so they stay out
/// unless the caller asks for them.
pub fn suppression_list_op(ctx: &Built, params: Value) -> Result<Value, VogtError> {
    let include_revoked = params
        .get("include_revoked")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let limit = params.get("limit").and_then(Value::as_i64).unwrap_or(100);
    crate::with_ctx!(ctx, |ctx| {
        let view = ctx.declared.read()?;
        let suppressions = view.list_suppressions(include_revoked, limit)?;
        Ok(json!({ "suppressions": suppressions }))
    })
}

/// Un-suppress a subject. Revoked, not deleted: the decision is history.
pub fn suppression_revoke_op(ctx: &Built, params: Value) -> Result<Value, VogtError> {
    let id = field(&params, "suppression.revoke", "id")?;
    let reason = field(&params, "suppression.revoke", "reason")?;
    crate::with_ctx!(ctx, |ctx| revoke(ctx, &id, &reason))
}

fn revoke<C: Clock + 'static, I: IdFactory + 'static>(
    ctx: &AppContext<C, I>,
    id: &str,
    reason: &str,
) -> Result<Value, VogtError> {
    let now = clock_now(&ctx.clock);
    let mut write = write_of(ctx);
    audited_write(&mut write, "suppression.revoke", reason, |txn, actor| {
        let existing = txn.suppression_by_id(id)?;
        if existing.is_none() {
            return Err(VogtError::NotFound(format!(
                "no suppression {}",
                crate::core::py_repr(id)
            )));
        }
        let revoked = txn.revoke_suppression(id, &actor.id, reason, now)?;
        if !revoked {
            return Err(VogtError::Conflict(format!(
                "suppression {} is already revoked",
                crate::core::py_repr(id)
            )));
        }
        let updated = txn
            .suppression_by_id(id)?
            .ok_or_else(|| VogtError::NotFound(format!("no suppression '{id}'")))?;
        Ok(WriteOutcome {
            result: json!({ "suppression": updated }),
            entity_kind: "suppression".to_string(),
            entity_id: id.to_string(),
            payload: serde_json::to_value(&updated).unwrap_or(Value::Null),
            event_kind: SUPPRESSION_REVOKED_EVENT.to_string(),
            summary: json!({ "subject": updated.subject_key_or_pattern }),
        })
    })
}

fn field(params: &Value, operation: &str, name: &str) -> Result<String, VogtError> {
    params
        .get(name)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| VogtError::InvalidRequest(format!("{operation} needs a {name}")))
}

fn clock_now<C: Clock>(clock: &std::sync::Arc<std::sync::Mutex<C>>) -> crate::core::Moment {
    clock
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .now()
}
