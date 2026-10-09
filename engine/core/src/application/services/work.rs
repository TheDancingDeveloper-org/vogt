//! `work.list` and the native half of `work.create`. Ports `list_work` and
//! `_create_native` in `services/work.py`.
//!
//! On a linked project the list is the forge mirror joined to the overlay; a
//! global list carries every linked project's upstream items alongside the
//! declared rows. An unlinked project scope answers with the CTA marker —
//! empty items and `link_state: "unlinked"` — rather than its native rows,
//! which stay reachable by ref.
//!
//! `work.create` with no project, or with `local_only`, writes a native
//! declared item. A linked project without `local_only` is the forge
//! write-through, which this port does not do: it fails typed rather than
//! storing a local item the forge never heard of. An unlinked project without
//! `local_only` is the decision-10 refusal.

use serde_json::{json, Value};

use crate::application::context::{AppContext, Built};
use crate::application::resolve;
use crate::application::upstream;
use crate::application::writes::{audited_write, WriteOutcome};
use crate::core::{
    Clock, Effort, IdFactory, Origin, Priority, Project, TrustState, WorkItem, WorkKind,
    TERMINAL_STATES,
};
use crate::errors::VogtError;
use crate::storage::interface::{DeclaredStore, ReadView, WorkFilter, WriteTxn};

const WORK_CREATE: &str = "work.create";
const WORK_CREATED_EVENT: &str = "work.created";

const UNLINKED_STILL_WORKS: &str = "Native items (WI-n) on this project still take comments, \
     transitions (including to done) and field edits by ref";

pub fn create_op(ctx: &Built, params: Value) -> Result<Value, VogtError> {
    crate::with_ctx!(ctx, |ctx| create_work(ctx, params))
}

pub fn list_op(ctx: &Built, params: Value) -> Result<Value, VogtError> {
    crate::with_ctx!(ctx, |ctx| list_work(ctx, params))
}

/// Create a work item. No project, or `local_only`, writes a native declared
/// item. A linked project without `local_only` is the forge write-through,
/// which is not ported and fails typed. An unlinked project without
/// `local_only` is the decision-10 refusal.
fn create_work<C, I>(ctx: &AppContext<C, I>, params: Value) -> Result<Value, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let params = parse_create(params)?;
    let project = match params.project.as_deref() {
        None => None,
        Some(slug) => Some(resolve::project(&ctx.declared.read()?, slug)?),
    };
    if params.project.is_none() || params.local_only {
        return create_native(ctx, &params, project.as_ref());
    }
    let project = project.expect("a project was resolved above");
    if !upstream::is_linked(&project) {
        return Err(refuse_unlinked(&project));
    }
    // Decision 9: the forge write-through runs before anything local exists.
    // It is not ported, and storing a native row here would be the local
    // success the forge never heard of.
    Err(VogtError::UpstreamWriteFailed(
        "work.create on a linked project writes through to the forge, and that path is not ported — nothing was stored locally. Pass local_only to keep a native item instead.".to_string(),
    ))
}

struct CreateParams {
    kind: String,
    title: String,
    body: String,
    priority: String,
    effort: Option<String>,
    project: Option<String>,
    initiative: Option<String>,
    assignee: Option<String>,
    labels: Vec<String>,
    local_only: bool,
    reason: String,
}

fn parse_create(params: Value) -> Result<CreateParams, VogtError> {
    let Value::Object(map) = params else {
        return Err(VogtError::InvalidRequest(
            "work.create takes an object".to_string(),
        ));
    };
    let text = |key: &str| match map.get(key) {
        None | Some(Value::Null) => None,
        Some(Value::String(value)) => Some(value.clone()),
        Some(_) => Some(String::new()),
    };
    let kind = text("kind").ok_or_else(|| VogtError::InvalidRequest("kind is required".into()))?;
    let title =
        text("title").ok_or_else(|| VogtError::InvalidRequest("title is required".into()))?;
    let priority = text("priority").unwrap_or_else(|| "p2".to_string());
    let labels = map
        .get("labels")
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    Ok(CreateParams {
        kind,
        title,
        body: text("body").unwrap_or_default(),
        priority,
        effort: text("effort"),
        project: text("project"),
        initiative: text("initiative"),
        assignee: text("assignee"),
        labels,
        local_only: map
            .get("local_only")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        reason: text("reason").unwrap_or_default(),
    })
}

fn refuse_unlinked(project: &Project) -> VogtError {
    VogtError::NotLinked(format!(
        "work.create needs a forge-linked project, and {} is not linked: link it (`forge link`, or re-import through `project import`) or publish it (`forge publish`) first; or pass `local_only: true` to keep a native item in this project. {UNLINKED_STILL_WORKS}.",
        crate::core::py_repr(&project.slug)
    ))
}

/// A native declared item, optionally scoped to a project. `project` set is the
/// `local_only` path: the row belongs to the project but has a `WI-n` ref and
/// `origin="created"`.
fn create_native<C, I>(
    ctx: &AppContext<C, I>,
    params: &CreateParams,
    project: Option<&Project>,
) -> Result<Value, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let kind: WorkKind = params.kind.parse().map_err(|_| {
        VogtError::InvalidRequest(format!(
            "unknown work kind {}",
            crate::core::py_repr(&params.kind)
        ))
    })?;
    let priority: Priority = params.priority.parse().map_err(|_| {
        VogtError::InvalidRequest(format!(
            "unknown priority {}",
            crate::core::py_repr(&params.priority)
        ))
    })?;
    let effort = params
        .effort
        .as_deref()
        .map(|value| {
            value.parse::<Effort>().map_err(|_| {
                VogtError::InvalidRequest(format!("unknown effort {}", crate::core::py_repr(value)))
            })
        })
        .transpose()?;
    let project_id = project.map(|project| project.id.clone());
    let drawn = Drawn {
        kind,
        title: params.title.clone(),
        body: params.body.clone(),
        priority,
        effort,
        project_id,
        initiative: params.initiative.clone(),
        assignee: params.assignee.clone(),
        labels: params.labels.clone(),
    };

    let mut writing = crate::application::context::write_of(ctx);
    let clock = std::sync::Arc::clone(writing.clock());
    let ids = std::sync::Arc::clone(writing.ids());
    let stored = audited_write(
        &mut writing,
        WORK_CREATE,
        &params.reason,
        move |txn, _actor| {
            let workflow = txn.workflow_for(&drawn.kind.to_string())?;
            let initiative_id = drawn
                .initiative
                .as_deref()
                .map(|slug| resolve::initiative(txn, slug).map(|found| found.id))
                .transpose()?;
            let assignee_id = drawn
                .assignee
                .as_deref()
                .map(|identity| resolve::actor(txn, identity).map(|found| found.id))
                .transpose()?;
            for name in &drawn.labels {
                resolve::label_exists(txn, name)?;
            }
            // Drawn inside the transaction, after the resolutions, which is where
            // Python reads it. The audit row reads the clock again afterwards.
            let now = super::now_of(&clock);
            let id = super::next_id(&ids, "wrk");
            let reference = txn.next_work_ref()?;
            let item = WorkItem {
                id: id.clone(),
                reference: reference.clone(),
                kind: drawn.kind,
                title: drawn.title.clone(),
                body: drawn.body.clone(),
                state: workflow.initial_state,
                priority: drawn.priority,
                effort: drawn.effort,
                project_id: drawn.project_id.clone(),
                project_slug: None,
                initiative_id,
                origin: Origin::Created,
                trust_state: TrustState::Unverified,
                assignee_actor_id: assignee_id,
                assignee_identity_ref: None,
                labels: drawn.labels.clone(),
                relations: Vec::new(),
                superseded_by: None,
                created_at: now,
                updated_at: now,
            };
            txn.insert_work_item(&item)?;
            let stored = txn.work_item_by_id(&id)?.ok_or_else(|| {
                VogtError::NotFound(format!("work item {id} vanished inside its own write"))
            })?;
            let payload = serde_json::to_value(&stored).unwrap_or(Value::Null);
            let summary = json!({"ref": reference, "kind": drawn.kind, "title": drawn.title});
            Ok(WriteOutcome::new(
                stored,
                "work_item",
                &id,
                payload,
                WORK_CREATED_EVENT,
                summary,
            ))
        },
    )?;
    Ok(json!({
        "item": stored,
        "comments": [],
        "sessions": [],
        "branches": [],
        "git": null,
        "walked": [],
        "live_sessions": [],
    }))
}

struct Drawn {
    kind: WorkKind,
    title: String,
    body: String,
    priority: Priority,
    effort: Option<Effort>,
    project_id: Option<String>,
    initiative: Option<String>,
    assignee: Option<String>,
    labels: Vec<String>,
}

fn list_work<C: Clock, I: IdFactory>(
    ctx: &AppContext<C, I>,
    params: Value,
) -> Result<Value, VogtError> {
    let params = aliases(params)?;
    let project = params.get("project").and_then(Value::as_str);
    let kinds = strings(params.get("kinds"));
    let states = strings(params.get("states"));
    let priorities = strings(params.get("priorities"));
    let assignee = params.get("assignee").and_then(Value::as_str);
    let initiative = params.get("initiative").and_then(Value::as_str);
    let label = params.get("label").and_then(Value::as_str);
    let query = params.get("query").and_then(Value::as_str);
    let include_finished = params
        .get("include_finished")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        || states
            .iter()
            .any(|state| TERMINAL_STATES.contains(&state.as_str()));
    let mode = params
        .get("mode")
        .and_then(Value::as_str)
        .unwrap_or("summary");
    let limit = params.get("limit").and_then(Value::as_i64).unwrap_or(50);
    let offset = params.get("offset").and_then(Value::as_i64).unwrap_or(0);

    let view = ctx.declared.read()?;
    let project_row = project
        .map(|slug| resolve::project(&view, slug))
        .transpose()?;
    if let Some(project_row) = project_row.as_ref() {
        if !upstream::is_linked(project_row) {
            let mut result = work_page(mode, offset, Vec::new(), 0, Some("unlinked"));
            result["detail"] = unlinked_detail(&view, project_row)?
                .map(Value::String)
                .unwrap_or(Value::Null);
            return Ok(result);
        }
    }
    let work_filter = WorkFilter {
        project_id: project_row.as_ref().map(|project| project.id.clone()),
        kinds,
        states,
        priorities,
        assignee_actor_id: assignee
            .map(|identity| resolve::actor(&view, identity).map(|actor| actor.id))
            .transpose()?,
        initiative_id: initiative
            .map(|slug| resolve::initiative(&view, slug).map(|initiative| initiative.id))
            .transpose()?,
        label: label.map(str::to_string),
        text: query.filter(|text| !text.is_empty()).map(str::to_string),
        exclude_terminal: !include_finished,
        limit,
        offset,
        ..WorkFilter::default()
    };
    let mut upstream_rows = Vec::new();
    for linked in upstream::linked_projects(&view, project_row.as_ref())? {
        for item in upstream::upstream_items(ctx, &view, &linked, include_finished, 1000)? {
            if upstream::matches(&item, &work_filter) {
                upstream_rows.push(item);
            }
        }
    }
    let link_state = project_row.as_ref().map(|_| "linked");
    if upstream_rows.is_empty() {
        let page = view.list_work_items(&work_filter)?;
        let total = view.count_work_items(&work_filter)?;
        return Ok(work_page(mode, offset, page, total, link_state));
    }
    // Merged paging: the declared page window cannot be pushed into SQL once
    // upstream rows join the list, so both halves are gathered and the one
    // ordering — (created_at, ref), the same the SQL uses — is sliced once.
    let unpaged = WorkFilter {
        limit: work_filter.limit + work_filter.offset + upstream_rows.len() as i64,
        offset: 0,
        ..work_filter.clone()
    };
    let declared_rows = view.list_work_items(&unpaged)?;
    let mut merged = declared_rows;
    merged.extend(upstream_rows.iter().cloned());
    merged.sort_by(|left, right| {
        left.created_at
            .cmp(&right.created_at)
            .then(left.reference.cmp(&right.reference))
    });
    let total = view.count_work_items(&work_filter)? + upstream_rows.len() as i64;
    let start = offset.max(0) as usize;
    let page = merged
        .into_iter()
        .skip(start)
        .take(limit.max(0) as usize)
        .collect();
    Ok(work_page(mode, offset, page, total, link_state))
}

fn work_page(
    mode: &str,
    offset: i64,
    page: Vec<WorkItem>,
    total: i64,
    link_state: Option<&str>,
) -> Value {
    let following = offset + page.len() as i64;
    let items: Vec<Value> = page
        .iter()
        .map(|item| {
            if mode == "full" {
                serde_json::to_value(item).unwrap_or(Value::Null)
            } else {
                json!({
                    "ref": item.reference,
                    "title": item.title,
                    "kind": item.kind.to_string(),
                    "state": item.state,
                    "priority": item.priority.to_string(),
                    "project_slug": item.project_slug,
                })
            }
        })
        .collect();
    // `WorkListResult`'s field order, every key present. A scoped list and a
    // global one carry the same shape; `link_state` and `detail` are null where
    // they do not apply, and `next_offset` is null on the last page.
    json!({
        "items": items,
        "total": total,
        "mode": mode,
        "next_offset": (!page.is_empty() && following < total).then_some(following),
        "link_state": link_state,
        "detail": Value::Null,
    })
}

/// What an unlinked project's empty list is hiding, and how to reach it. An
/// agent reading only `items: []` would learn that its own native items do not
/// exist; naming the count and a few refs keeps the CTA while pointing at the
/// by-ref path that still works.
fn unlinked_detail(view: &impl ReadView, project: &Project) -> Result<Option<String>, VogtError> {
    let work_filter = WorkFilter {
        project_id: Some(project.id.clone()),
        exclude_terminal: true,
        limit: 5,
        ..WorkFilter::default()
    };
    let total = view.count_work_items(&work_filter)?;
    if total == 0 {
        return Ok(None);
    }
    let refs = view
        .list_work_items(&work_filter)?
        .iter()
        .map(|item| item.reference.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    Ok(Some(format!(
        "{} is not forge-linked, so its {total} open native item(s) are not listed here \
         (e.g. {refs}). {UNLINKED_STILL_WORKS}: work.get, work.comment and work.transition \
         them directly; link or publish the project to list them.",
        crate::core::py_repr(&project.slug)
    )))
}

/// `apply_aliases` plus the comma-split of a string `states`. An alias is
/// accepted only when the canonical field is absent; naming both is refused.
fn aliases(params: Value) -> Result<Value, VogtError> {
    let Value::Object(mut map) = params else {
        return Ok(params);
    };
    for (alias, field) in [
        ("status", "states"),
        ("state", "states"),
        ("text", "query"),
        ("search", "query"),
        ("q", "query"),
    ] {
        let Some(value) = map.remove(alias) else {
            continue;
        };
        if map.contains_key(field) {
            return Err(VogtError::InvalidRequest(format!(
                "work.list: {} is an alias of {field:?}; pass one of them, not both",
                crate::core::py_repr(alias)
            )));
        }
        map.insert(field.to_string(), value);
    }
    if let Some(Value::String(states)) = map.get("states") {
        let split: Vec<Value> = states
            .split(',')
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .map(|part| Value::String(part.to_string()))
            .collect();
        map.insert("states".to_string(), Value::Array(split));
    }
    Ok(Value::Object(map))
}

fn strings(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::work_page;
    use serde_json::Value;

    #[test]
    fn the_envelope_carries_every_key_in_the_model_order() {
        // `WorkListResult`'s field order, with the nulls present. A global list
        // has no link state and an empty page has no next offset, and neither
        // is a reason to drop the key.
        let page = work_page("summary", 0, Vec::new(), 0, None);
        let keys: Vec<&String> = page.as_object().unwrap().keys().collect();
        assert_eq!(
            keys,
            [
                "items",
                "total",
                "mode",
                "next_offset",
                "link_state",
                "detail"
            ]
        );
        assert_eq!(page["next_offset"], Value::Null);
        assert_eq!(page["link_state"], Value::Null);
        assert_eq!(page["detail"], Value::Null);

        let scoped = work_page("full", 0, Vec::new(), 0, Some("unlinked"));
        let scoped_keys: Vec<&String> = scoped.as_object().unwrap().keys().collect();
        assert_eq!(scoped_keys, keys);
        assert_eq!(scoped["link_state"], "unlinked");
    }
}
