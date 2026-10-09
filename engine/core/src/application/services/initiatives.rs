//! Initiatives. Ports the initiative half of `taxonomy.py`, plus
//! `initiative.publish`.
//!
//! `initiative.publish` writes a forge tracking issue. `Built` carries no forge
//! client — the provider trait exists, but nothing on the context holds one,
//! and this module is not allowed to edit the adapters to add one. The publish
//! path therefore resolves the initiative and then refuses, loudly, rather than
//! pretend a tracking issue was written. `reproject_initiative` is the
//! adopt-only refresh that follows a title or body change; with no forge client
//! it is a no-op, which is the best-effort the Python already promises.

use serde_json::{json, Value};

use crate::application::context::{AppContext, Built};
use crate::application::services::preferences::{optional_i64, optional_string, require_string};
use crate::application::services::{context_parts, dispatch, next_id, now_of, write_context};
use crate::application::writes::{audited_write, WriteOutcome};
use crate::core::{slugify, Clock, IdFactory, Initiative, InitiativeState};
use crate::errors::VogtError;
use crate::storage::interface::{DeclaredStore, ReadView, WriteTxn};
use std::sync::Arc;

pub fn initiative_create_op(built: &Built, params: Value) -> Result<Value, VogtError> {
    dispatch!(built, initiative_create, params)
}

pub fn initiative_list_op(built: &Built, params: Value) -> Result<Value, VogtError> {
    dispatch!(built, initiative_list, params)
}

pub fn initiative_update_op(built: &Built, params: Value) -> Result<Value, VogtError> {
    dispatch!(built, initiative_update, params)
}

pub fn initiative_publish_op(built: &Built, params: Value) -> Result<Value, VogtError> {
    dispatch!(built, initiative_publish, params)
}

fn initiative_create<C, I>(ctx: &AppContext<C, I>, params: Value) -> Result<Value, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let title = require_string(&params, "title")?;
    let body = optional_string(&params, "body")?.unwrap_or_default();
    let state = optional_string(&params, "state")?.unwrap_or_else(|| "open".to_string());
    let weight = optional_i64(&params, "weight", 0)?;
    let reason = require_string(&params, "reason")?;
    let slug = slugify(&title);
    if slug.is_empty() {
        return Err(VogtError::InvalidRequest(
            "initiative title does not yield a slug".to_string(),
        ));
    }
    let state = parse_state(&state)?;

    let (declared, _, principal, clock, ids, _) = context_parts(ctx);
    let now = now_of(&clock);
    let id = next_id(&ids, "ini");
    let mut writing = write_context(declared, principal, Arc::clone(&clock), Arc::clone(&ids));
    let made = insert_initiative(
        &mut writing,
        &reason,
        Initiative {
            id,
            slug,
            title,
            body,
            state,
            weight,
            created_at: now,
            updated_at: now,
        },
    )?;
    Ok(json!({"initiative": initiative_json(&made)}))
}

/// The audited half, against the concrete store. A closure built against the
/// generic `DeclaredStore` bound does not compile: the compiler reads the
/// clock and id lifetimes as `'static`.
fn insert_initiative<C: Clock + 'static, I: IdFactory + 'static>(
    writing: &mut crate::application::writes::WriteContext<
        '_,
        C,
        I,
        crate::storage::sqlite::declared::SqliteDeclaredStore<C, I>,
    >,
    reason: &str,
    made: Initiative,
) -> Result<Initiative, VogtError> {
    audited_write(writing, "initiative.create", reason, |txn, _actor| {
        if txn.initiative_by_slug(&made.slug)?.is_some() {
            return Err(VogtError::Conflict(format!(
                "an initiative with slug {} already exists",
                crate::core::py_repr(&made.slug),
            )));
        }
        txn.insert_initiative(&made)?;
        Ok(WriteOutcome::new(
            made.clone(),
            "initiative",
            &made.id,
            initiative_json(&made),
            "initiative.created",
            json!({"slug": made.slug, "weight": made.weight}),
        ))
    })
}

fn initiative_list<C, I>(ctx: &AppContext<C, I>, params: Value) -> Result<Value, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let limit = optional_i64(&params, "limit", 100)?;
    let offset = optional_i64(&params, "offset", 0)?;
    let (declared, _, _, _, _, _) = context_parts(ctx);
    let view = declared.read()?;
    let rows = view.list_initiatives(limit, offset)?;
    Ok(json!({
        "initiatives": rows.iter().map(initiative_json).collect::<Vec<_>>(),
    }))
}

fn initiative_update<C, I>(ctx: &AppContext<C, I>, params: Value) -> Result<Value, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let slug = require_string(&params, "slug")?;
    let title = optional_string(&params, "title")?;
    let body = match params.get("body") {
        None | Some(Value::Null) => None,
        Some(Value::String(text)) => Some(text.clone()),
        Some(_) => {
            return Err(VogtError::InvalidRequest(
                "body must be a string".to_string(),
            ))
        }
    };
    let weight =
        match params.get("weight") {
            None | Some(Value::Null) => None,
            Some(Value::Number(number)) => Some(number.as_i64().ok_or_else(|| {
                VogtError::InvalidRequest("weight must be an integer".to_string())
            })?),
            Some(_) => {
                return Err(VogtError::InvalidRequest(
                    "weight must be an integer".to_string(),
                ))
            }
        };
    let state = match optional_string(&params, "state")? {
        Some(text) => Some(parse_state(&text)?),
        None => None,
    };
    let reason = require_string(&params, "reason")?;
    if title.is_none() && body.is_none() && weight.is_none() && state.is_none() {
        return Err(VogtError::InvalidRequest(
            "give a title, body, weight or state; there is nothing else to update".to_string(),
        ));
    }
    let changed_text = title.is_some() || body.is_some();

    let (declared, _, principal, clock, ids, _) = context_parts(ctx);
    let now = now_of(&clock);
    let mut writing = write_context(declared, principal, Arc::clone(&clock), Arc::clone(&ids));
    let updated = apply_initiative_update(
        &mut writing,
        &reason,
        &InitiativeChange {
            slug,
            title,
            body,
            weight,
            state,
            now,
        },
    )?;
    if changed_text {
        // Best-effort, and adopt-only. There is no forge client on the context,
        // so the reprojection is a no-op rather than a second error.
        reproject_initiative();
    }
    Ok(json!({"initiative": initiative_json(&updated)}))
}

/// The fields an update may change, gathered so the audit closure takes one
/// argument.
struct InitiativeChange {
    slug: String,
    title: Option<String>,
    body: Option<String>,
    weight: Option<i64>,
    state: Option<InitiativeState>,
    now: crate::core::Moment,
}

fn apply_initiative_update<C: Clock + 'static, I: IdFactory + 'static>(
    writing: &mut crate::application::writes::WriteContext<
        '_,
        C,
        I,
        crate::storage::sqlite::declared::SqliteDeclaredStore<C, I>,
    >,
    reason: &str,
    change: &InitiativeChange,
) -> Result<Initiative, VogtError> {
    let slug = change.slug.clone();
    let title = change.title.clone();
    let body = change.body.clone();
    let weight = change.weight;
    let state = change.state;
    let now = change.now;
    audited_write(writing, "initiative.update", reason, |txn, _actor| {
        let mut current = txn.initiative_by_slug(&slug)?.ok_or_else(|| {
            VogtError::NotFound(format!(
                "no initiative with slug {}",
                crate::core::py_repr(&slug)
            ))
        })?;
        if let Some(title) = &title {
            current.title = title.clone();
        }
        if let Some(body) = &body {
            current.body = body.clone();
        }
        if let Some(weight) = weight {
            current.weight = weight;
        }
        if let Some(state) = state {
            current.state = state;
        }
        current.updated_at = now;
        txn.update_initiative(&current)?;
        let mut changed: Vec<&str> = Vec::new();
        if title.is_some() {
            changed.push("title");
        }
        if body.is_some() {
            changed.push("body");
        }
        if weight.is_some() {
            changed.push("weight");
        }
        if state.is_some() {
            changed.push("state");
        }
        changed.sort_unstable();
        Ok(WriteOutcome::new(
            current.clone(),
            "initiative",
            &current.id,
            initiative_json(&current),
            "initiative.updated",
            json!({"slug": current.slug, "fields": changed}),
        ))
    })
}

/// `initiative.publish`. With no forge-linked projects the initiative spans,
/// there is nothing to project and no forge client is needed: the result is an
/// empty tracking list and the action is still audited. A linked project would
/// need the forge adapter, which this build does not carry, so that case
/// refuses with the not-ported error rather than skipping the repo silently.
fn initiative_publish<C, I>(ctx: &AppContext<C, I>, params: Value) -> Result<Value, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let slug = require_string(&params, "slug")?;
    let reason = require_string(&params, "reason")?;
    let (declared, _, principal, clock, ids, _) = context_parts(ctx);
    let (initiative, linked) = {
        let view = declared.read()?;
        let initiative = view.initiative_by_slug(&slug)?.ok_or_else(|| {
            VogtError::NotFound(format!(
                "no initiative with slug {}",
                crate::core::py_repr(&slug)
            ))
        })?;
        let linked = view
            .list_projects(10_000, 0)?
            .into_iter()
            .any(|project| project.link_state == crate::core::LinkState::Linked);
        (initiative, linked)
    };
    if linked {
        return Err(VogtError::InvalidRequest(
            "initiative.publish is not available in this build: its service has not been ported yet"
                .to_string(),
        ));
    }
    let result = json!({
        "slug": initiative.slug,
        "state": initiative.state.to_string(),
        "tracking_issues": [],
    });
    let mut writing = write_context(declared, principal, Arc::clone(&clock), Arc::clone(&ids));
    crate::application::writes::audited_action(
        &mut writing,
        "initiative.publish",
        &reason,
        "initiative",
        &initiative.id,
        &result,
        "initiative.projected",
        None,
    )?;
    Ok(result)
}

/// The adopt-only refresh of an initiative's tracking issue. With no forge
/// client there is nothing to refresh, and the Python already treats a failed
/// reprojection as best-effort.
fn reproject_initiative() {}

fn parse_state(text: &str) -> Result<InitiativeState, VogtError> {
    match text {
        "open" => Ok(InitiativeState::Open),
        "closed" => Ok(InitiativeState::Closed),
        other => Err(VogtError::InvalidRequest(format!(
            "initiative state {other:?} is not open or closed"
        ))),
    }
}

fn initiative_json(initiative: &Initiative) -> Value {
    json!({
        "id": initiative.id,
        "slug": initiative.slug,
        "title": initiative.title,
        "body": initiative.body,
        "state": initiative.state.to_string(),
        "weight": initiative.weight,
        "created_at": initiative.created_at.to_json(),
        "updated_at": initiative.updated_at.to_json(),
    })
}
