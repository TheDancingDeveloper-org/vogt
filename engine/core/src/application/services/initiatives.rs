//! Initiatives. Ports the initiative half of `taxonomy.py`, plus
//! `initiative.publish`.
//!
//! `initiative.publish` writes a forge tracking issue. `Built` carries no forge
//! client — the provider trait exists, but nothing on the context holds one,
//! and this module is not allowed to edit the adapters to add one. The publish
//! path therefore resolves the initiative and then refuses, loudly, rather than
//! pretend a tracking issue was written. `reproject_initiative` is the
//! adopt-only refresh that follows a title or body change, or a `work.update`
//! that moves an item into or out of the initiative: it re-renders tracking
//! issues that already carry the initiative's marker, and never opens one.

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
    let mut writing = write_context(declared, principal, Arc::clone(&clock), Arc::clone(&ids));
    let made = insert_initiative(
        &mut writing,
        &reason,
        &ids,
        Initiative {
            id: String::new(),
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
    ids: &std::sync::Arc<std::sync::Mutex<I>>,
    mut made: Initiative,
) -> Result<Initiative, VogtError> {
    audited_write(writing, "initiative.create", reason, |txn, _actor| {
        if txn.initiative_by_slug(&made.slug)?.is_some() {
            return Err(VogtError::Conflict(format!(
                "an initiative with slug {} already exists",
                crate::core::py_repr(&made.slug),
            )));
        }
        // Minted after the duplicate check, as Python's id factory runs inside
        // the write body: a rejected duplicate must not burn an `ini_` id.
        made.id = next_id(ids, "ini");
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
    // `summary` blanks each body. Anything else, including the default `full`,
    // keeps it. Python copies the row and sets body to "" only in that case.
    let summary = params.get("mode").and_then(Value::as_str) == Some("summary");
    let initiatives = rows
        .iter()
        .map(|row| {
            let mut shown = row.clone();
            if summary {
                shown.body.clear();
            }
            initiative_json(&shown)
        })
        .collect::<Vec<_>>();
    Ok(json!({ "initiatives": initiatives }))
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
        // Adopt-only: it re-renders tracking issues that already exist.
        reproject_initiative(ctx, &updated.id, &reason)?;
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
        return Err(VogtError::NotImplemented(
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

/// Re-render existing tracking issues after a membership or text change.
/// Ports `initiative_publish.reproject_initiative`.
///
/// Adopt-only: it never opens a tracking issue, so a change on an initiative
/// nobody has published yet is a no-op that writes nothing. The local write
/// has already committed; a forge that refuses the edit surfaces as
/// `UpstreamWriteFailed`, as the Python's `_projection_failed` does, without
/// unwinding it.
pub(crate) fn reproject_initiative<C, I>(
    ctx: &AppContext<C, I>,
    initiative_id: &str,
    reason: &str,
) -> Result<(), VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let (initiative, targets) = {
        let view = ctx.declared.read()?;
        let Some(initiative) = view.initiative_by_id(initiative_id)? else {
            return Ok(());
        };
        let actor = view.actor_by_identity(&ctx.principal.identity_ref)?;
        let targets = projection_targets(ctx, &view, actor.as_ref(), &initiative)?;
        (initiative, targets)
    };

    let mut placed = Vec::new();
    for target in targets {
        if let Some(found) = find_existing(target, &initiative)? {
            placed.push(found);
        }
    }
    if placed.is_empty() {
        return Ok(());
    }

    let rows = reproject_placed(&initiative, &placed)?;
    let outcome = json!({
        "slug": initiative.slug,
        "state": initiative.state.to_string(),
        "tracking_issues": rows,
    });
    let mut writing = crate::application::context::write_of(ctx);
    crate::application::writes::audited_action(
        &mut writing,
        "initiative.publish",
        reason,
        "initiative",
        &initiative.id,
        &outcome,
        "initiative.reprojected",
        None,
    )
}

/// One linked repo the initiative spans, resolved and writable. Ports `_Target`.
struct ProjectionTarget {
    project: crate::core::Project,
    provider: crate::adapters::forge::registry::ForgeClient,
    identity: String,
    repo: crate::adapters::forge::RepoRef,
    members: Vec<crate::core::WorkItem>,
}

/// A tracking issue found by its marker. Ports `_Placed` (adopted only).
struct PlacedIssue {
    target: ProjectionTarget,
    number: i64,
    source_url: Option<String>,
    current_body: String,
}

/// Far above any plausible initiative membership. Ports `_MEMBER_LIMIT`.
const MEMBER_LIMIT: i64 = 10_000;

/// The linked projects holding a member that can be written. Ports `_targets`;
/// the skipped rows only matter to `initiative.publish`, so they are dropped.
fn projection_targets<C, I>(
    ctx: &AppContext<C, I>,
    view: &impl ReadView,
    actor: Option<&crate::core::Actor>,
    initiative: &Initiative,
) -> Result<Vec<ProjectionTarget>, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    use crate::adapters::forge::ForgeProvider;
    let mut targets = Vec::new();
    for project in crate::application::upstream::linked_projects(view, None)? {
        let members: Vec<_> =
            crate::application::upstream::upstream_items(ctx, view, &project, true, MEMBER_LIMIT)?
                .into_iter()
                .filter(|item| item.initiative_id.as_deref() == Some(initiative.id.as_str()))
                .collect();
        if members.is_empty() {
            continue;
        }
        if !crate::adapters::forge::permits(&project.write_back.to_string(), "create") {
            continue;
        }
        let (provider, identity) = crate::application::services::forge_writeback::writer_provider(
            ctx,
            actor,
            project.repo_url.as_deref(),
        )?;
        let Some(provider) = provider else {
            continue;
        };
        let Some(repo) = provider.parse(project.repo_url.as_deref()) else {
            continue;
        };
        let numbered = members
            .into_iter()
            .filter(|item| provider.number_of(Some(&item.reference)).is_some())
            .collect();
        targets.push(ProjectionTarget {
            project,
            provider,
            identity,
            repo,
            members: numbered,
        });
    }
    Ok(targets)
}

/// The tracking issue Vogt already owns in this repo, adopted by marker.
fn find_existing(
    target: ProjectionTarget,
    initiative: &Initiative,
) -> Result<Option<PlacedIssue>, VogtError> {
    use crate::adapters::forge::ForgeProvider;
    use crate::adapters::forge::IssuesItem;
    let issues = target.provider.issues_updated_since(&target.repo, None)?;
    for issue in issues.into_iter().filter_map(IssuesItem::into_issue) {
        if crate::decisions::body_has_marker(issue.body.as_deref(), &initiative.slug) {
            return Ok(Some(PlacedIssue {
                number: issue.number,
                source_url: issue.source_url,
                current_body: issue.body.unwrap_or_default(),
                target,
            }));
        }
    }
    Ok(None)
}

fn task_lines(target: &ProjectionTarget) -> Vec<crate::decisions::TaskLine> {
    use crate::adapters::forge::ForgeProvider;
    let mut lines: Vec<_> = target
        .members
        .iter()
        .filter_map(|item| {
            target
                .provider
                .number_of(Some(&item.reference))
                .map(|number| {
                    crate::decisions::TaskLine::from_state(number, &item.title, &item.state)
                })
        })
        .collect();
    lines.sort_by_key(|line| line.number);
    lines
}

/// Render each managed region with its siblings and edit only a changed body.
/// Ports `_reproject_placed` with `propose_close=False`.
fn reproject_placed(
    initiative: &Initiative,
    placed: &[PlacedIssue],
) -> Result<Vec<Value>, VogtError> {
    use crate::adapters::forge::ForgeProvider;
    // A dict keyed by project slug: a later repo of the same slug wins, as in Python.
    let mut siblings: Vec<(String, String)> = Vec::new();
    for item in placed {
        if let Some(url) = item.source_url.as_deref().filter(|url| !url.is_empty()) {
            let slug = item.target.project.slug.clone();
            if let Some(slot) = siblings.iter_mut().find(|(known, _)| *known == slug) {
                slot.1 = url.to_owned();
            } else {
                siblings.push((slug, url.to_owned()));
            }
        }
    }
    let mut rows = Vec::new();
    for item in placed {
        let mut others: Vec<(&str, &str)> = siblings
            .iter()
            .filter(|(slug, _)| *slug != item.target.project.slug)
            .map(|(slug, url)| (slug.as_str(), url.as_str()))
            .collect();
        others.sort();
        let region = crate::decisions::render_managed_region(
            &initiative.slug,
            &initiative.body,
            &task_lines(&item.target),
            &others,
        );
        let new_body = crate::decisions::splice_managed_region(Some(&item.current_body), &region);
        let mut detail = Value::Null;
        if new_body != item.current_body {
            let sent = item.target.provider.update_issue_body(
                &item.target.repo,
                item.number,
                &new_body,
            )?;
            if sent.outcome != crate::adapters::forge::WriteBackOutcome::Succeeded {
                return Err(VogtError::UpstreamWriteFailed(format!(
                    "projecting the initiative onto {} failed: {} — nothing was closed or deleted, and re-running `initiative publish` resumes",
                    crate::core::py_repr(&item.target.project.slug),
                    sent.detail.as_deref().unwrap_or("no detail from the provider"),
                )));
            }
            detail = json!(format!("as {}", item.target.identity));
        }
        rows.push(json!({
            "project_slug": item.target.project.slug,
            "repo_url": item.target.project.repo_url,
            "number": item.number,
            "source_url": item.source_url,
            "action": "adopted",
            "members": item.target.members.len(),
            "detail": detail,
            "close_proposed": false,
        }));
    }
    Ok(rows)
}

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
