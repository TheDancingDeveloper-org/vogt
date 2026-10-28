//! Charters: standing instructions per scope (WI-2038). Ports the read half of
//! `application/services/charters.py`.
//!
//! A charter is a short Markdown text attached to the workspace, a project or
//! an initiative. Each set appends the next version; the current charter is
//! the highest version and an empty body clears it. Writes stay unported:
//! only `charter.get` is served here.

use serde_json::{json, Value};

use crate::application::context::{AppContext, Built};
use crate::core::{Clock, IdFactory};
use crate::errors::VogtError;
use crate::storage::interface::{DeclaredStore, ReadView};

pub fn charter_get_op(built: &Built, params: Value) -> Result<Value, VogtError> {
    crate::with_ctx!(built, |ctx| get(ctx, &params))
}

fn get<C: Clock + 'static, I: IdFactory + 'static>(
    ctx: &AppContext<C, I>,
    params: &Value,
) -> Result<Value, VogtError> {
    let scope = text(params, "scope");
    let reference = text_opt(params, "ref");
    let history = params
        .get("history")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let limit = params.get("limit").and_then(Value::as_i64).unwrap_or(20);
    let view = ctx.declared.read()?;
    let (scope_id, shown_ref) = resolve(&view, &scope, reference.as_deref())?;
    let current = view.charter(&scope, &scope_id)?;
    let history_rows = if history {
        view.charter_history(&scope, &scope_id, limit)?
    } else {
        Vec::new()
    };
    let current = match current {
        Some(row) => Some(view_of(&view, &scope, shown_ref.as_deref(), &row)?),
        None => None,
    };
    let history = history_rows
        .iter()
        .map(|row| view_of(&view, &scope, shown_ref.as_deref(), row))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(json!({ "current": current, "history": history }))
}

/// Workspace takes no ref and stores `scope_id` "". Project and initiative
/// scopes need the slug and store the entity id.
fn resolve(
    view: &impl ReadView,
    scope: &str,
    reference: Option<&str>,
) -> Result<(String, Option<String>), VogtError> {
    match scope {
        "workspace" => {
            if reference.is_some_and(|value| !value.is_empty()) {
                return Err(VogtError::InvalidRequest(
                    "the workspace charter takes no ref".to_string(),
                ));
            }
            Ok((String::new(), None))
        }
        "project" | "initiative" => {
            let Some(slug) = reference.filter(|value| !value.is_empty()) else {
                return Err(VogtError::InvalidRequest(format!(
                    "a {scope} charter needs ref (the {scope} slug)"
                )));
            };
            let id = if scope == "project" {
                view.project_by_slug(slug)?
                    .ok_or_else(|| {
                        VogtError::NotFound(format!(
                            "no project with slug {}",
                            crate::core::py_repr(slug)
                        ))
                    })?
                    .id
            } else {
                view.initiative_by_slug(slug)?
                    .ok_or_else(|| {
                        VogtError::NotFound(format!(
                            "no initiative with slug {}",
                            crate::core::py_repr(slug)
                        ))
                    })?
                    .id
            };
            Ok((id, Some(slug.to_string())))
        }
        other => Err(VogtError::InvalidRequest(format!(
            "unknown charter scope {other}"
        ))),
    }
}

fn view_of(
    view: &impl ReadView,
    scope: &str,
    shown_ref: Option<&str>,
    row: &crate::core::Charter,
) -> Result<Value, VogtError> {
    let author = match view.actor_by_id(&row.author_actor_id)? {
        Some(actor) => actor.identity_ref,
        None => row.author_actor_id.clone(),
    };
    Ok(json!({
        "scope": scope,
        "ref": shown_ref,
        "version": row.version,
        "body": row.body,
        "author": author,
        "created_at": crate::core::to_iso(row.created_at),
        "reason": row.reason,
    }))
}

fn text(params: &Value, name: &str) -> String {
    params
        .get(name)
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string()
}

fn text_opt(params: &Value, name: &str) -> Option<String> {
    params
        .get(name)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}
