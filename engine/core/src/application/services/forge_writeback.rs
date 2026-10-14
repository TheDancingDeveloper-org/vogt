//! `forge.writeback` and `forge.writeback_list`. Ports `writeback.py`'s
//! `set_write_back` and `list_write_backs`.
//!
//! The policy is a fact a person sets; nothing enforces it. The consequence
//! half — the part that posts upstream — lives in `adapters/forge/writeback.rs`
//! and is not this operation.

use serde_json::{json, Value};

use crate::application::context::{write_of, AppContext, Built};
use crate::application::services::{context_parts, dispatch, now_of};
use crate::application::writes::{audited_write, WriteOutcome};
use crate::core::{Clock, IdFactory, WriteBack};
use crate::errors::VogtError;
use crate::storage::interface::{DeclaredStore, ProjectUpdate, ReadView, WriteTxn};

pub const FORGE_WRITEBACK_SET_EVENT: &str = "forge.writeback_set";

pub fn writeback_op(built: &Built, params: Value) -> Result<Value, VogtError> {
    dispatch!(built, set_write_back, params)
}

pub fn writeback_list_op(built: &Built, params: Value) -> Result<Value, VogtError> {
    dispatch!(built, list_write_backs, params)
}

/// Set a project's write-back policy. The project must be linked first:
/// a policy on an unlinked project would describe a behaviour that cannot
/// happen, which is worse than refusing.
fn set_write_back<C, I>(ctx: &AppContext<C, I>, params: Value) -> Result<Value, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let slug = present_string(&params, "project")?;
    let policy = present_string(&params, "policy")?;
    let reason = present_string(&params, "reason")?;
    policy
        .parse::<WriteBack>()
        .map_err(|_| VogtError::InvalidRequest(format!("write-back policy {policy:?}")))?;
    let (declared, _, _, _, _, _) = context_parts(ctx);
    let project = resolve_project(&declared.read()?, &slug)?;
    if project.link_state.to_string() != "linked" {
        return Err(VogtError::NotLinked(format!(
            "project {slug:?} is not forge-linked; link it before setting a \
             write-back policy"
        )));
    }
    let previous = project.write_back.to_string();
    let id = project.id.clone();
    let mut writing = write_of(ctx);
    let updated = audited_write(&mut writing, "forge.writeback", &reason, |txn, _actor| {
        txn.update_project(
            &id,
            &ProjectUpdate {
                write_back: Some(policy.clone()),
                ..ProjectUpdate::default()
            },
            now_of(&ctx.clock),
        )?;
        let updated = txn
            .project_by_slug(&slug)?
            .ok_or_else(|| VogtError::NotFound(format!("no project {slug:?}")))?;
        Ok(WriteOutcome::new(
            json!({"project": updated}),
            "project",
            &id,
            json!({"write_back": policy}),
            FORGE_WRITEBACK_SET_EVENT,
            json!({"from": previous, "to": policy}),
        ))
    })?;
    Ok(updated)
}

/// What write-back has done, newest first. A read, not a mutation: the
/// record is the evidence, and reading it changes nothing.
fn list_write_backs<C, I>(ctx: &AppContext<C, I>, params: Value) -> Result<Value, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let outcome = optional_string(&params, "outcome")?;
    if let Some(outcome) = &outcome {
        if !["attempted", "succeeded", "failed", "skipped"].contains(&outcome.as_str()) {
            return Err(VogtError::InvalidRequest(format!(
                "write-back outcome {outcome:?}"
            )));
        }
    }
    let limit = match params.get("limit") {
        None | Some(Value::Null) => 100,
        Some(Value::Number(number)) => number.as_i64().unwrap_or(100),
        Some(_) => {
            return Err(VogtError::InvalidRequest(
                "limit must be a number".to_owned(),
            ))
        }
    };
    let (declared, _, _, _, _, _) = context_parts(ctx);
    let records = declared
        .read()?
        .list_writeback_actions(outcome.as_deref(), limit)?;
    Ok(json!({
        "actions": records
            .iter()
            .map(|record| json!({
                "id": record.id,
                "at": record.at,
                "action": record.action,
                "subject_key": record.subject_key,
                "policy": record.policy,
                "outcome": record.outcome,
                "reason": record.reason,
                "detail": record.detail,
                "source_url": record.source_url,
            }))
            .collect::<Vec<_>>(),
    }))
}

fn resolve_project(view: &impl ReadView, slug: &str) -> Result<crate::core::Project, VogtError> {
    view.project_by_slug(slug)?
        .ok_or_else(|| VogtError::NotFound(format!("no project {slug:?}")))
}

fn present_string(params: &Value, field: &str) -> Result<String, VogtError> {
    match params.get(field) {
        None | Some(Value::Null) => Err(VogtError::InvalidRequest(format!("{field} is required"))),
        Some(Value::String(text)) if text.trim().is_empty() => {
            Err(VogtError::InvalidRequest(format!("{field} is required")))
        }
        Some(Value::String(text)) => Ok(text.clone()),
        Some(_) => Err(VogtError::InvalidRequest(format!(
            "{field} must be a string"
        ))),
    }
}

fn optional_string(params: &Value, field: &str) -> Result<Option<String>, VogtError> {
    match params.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => Ok(Some(text.clone())),
        Some(_) => Err(VogtError::InvalidRequest(format!(
            "{field} must be a string"
        ))),
    }
}
