//! `work.list`, the native half of `work.create`, and the native half of
//! `work.update`. Ports `list_work`, `_create_native` and `_update_native` in
//! `services/work.py`.
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
use crate::storage::interface::{DeclaredStore, ReadView, WorkFilter, WorkItemUpdate, WriteTxn};

const WORK_CREATE: &str = "work.create";
const WORK_CREATED_EVENT: &str = "work.created";
const WORK_UPDATE: &str = "work.update";
const WORK_UPDATED_EVENT: &str = "work.updated";

const UNLINKED_STILL_WORKS: &str = "Native items (WI-n) on this project still take comments, \
     transitions (including to done) and field edits by ref";

pub fn create_op(ctx: &Built, params: Value) -> Result<Value, VogtError> {
    crate::with_ctx!(ctx, |ctx| create_work(ctx, params))
}

pub fn update_op(ctx: &Built, params: Value) -> Result<Value, VogtError> {
    crate::with_ctx!(ctx, |ctx| update_work(ctx, params))
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
    // A project was resolved above, so this is a project-scoped create.
    let Some(project) = project else {
        return Err(VogtError::InvalidRequest(
            "work.create resolved no project".to_string(),
        ));
    };
    if !upstream::is_linked(&project) {
        return Err(refuse_unlinked(&project, "work.create"));
    }
    // Decision 9: the forge write-through runs before anything local exists.
    // The precondition chain is local — policy, then a forge credential — and
    // a gap refuses the whole operation. Only a chain that would actually
    // reach the forge falls through to the not-ported failure, and that still
    // stores nothing.
    let gaps = write_through_gaps(ctx, &project, "create");
    if !gaps.is_empty() {
        return Err(refuse_gaps(&project, "create", &gaps, true));
    }
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

fn refuse_unlinked(project: &Project, operation: &str) -> VogtError {
    refuse_unlinked_for(project, operation)
}

/// Decision 10's refusal. `operation` is the phrase the message leads with:
/// `work.create`, or `work.update with labels`.
fn refuse_unlinked_for(project: &Project, operation: &str) -> VogtError {
    VogtError::NotLinked(format!(
        "{operation} needs a forge-linked project, and {} is not linked: link it (`forge link`, or re-import through `project import`) or publish it (`forge publish`) first; or pass `local_only: true` to keep a native item in this project. {UNLINKED_STILL_WORKS}.",
        crate::core::py_repr(&project.slug)
    ))
}

/// Every unmet precondition for a write-through, named, in the order Python
/// reports them. Ports `_gaps_for`.
///
/// The chain is the policy, then a forge credential, then whether the
/// project's `repo_url` parses for that provider. Credential resolution
/// (`writeback._writer_provider`) is not ported: with no provider wired, the
/// credential gate is unmet and the parse gate never runs, which is exactly
/// what an instance with no token answers. A chain that passes every gate is
/// empty, and that is the case the caller still has to fail as not-ported.
fn write_through_gaps<C: Clock, I: IdFactory>(
    _ctx: &AppContext<C, I>,
    project: &Project,
    action: &str,
) -> Vec<String> {
    let mut gaps = Vec::new();
    let policy = project.write_back.to_string();
    if !crate::adapters::forge::permits(&policy, action) {
        gaps.push(format!(
            "write-back policy is {}, which does not permit {} — set it with `forge writeback`",
            crate::core::py_repr(&policy),
            crate::core::py_repr(action),
        ));
    }
    // No provider resolves: the port has neither an actor PAT nor a file-token
    // provider, so the credential gate is the honest answer rather than a
    // guessed pass.
    gaps.push(
        "no forge credential resolves for this project — link your forge account with `forge account link`, or configure the instance token file".to_string(),
    );
    gaps
}

/// `_require_permitted`'s message: every gap numbered, with the create escape
/// hatch when `offer_local_only` is set.
fn refuse_gaps(
    project: &Project,
    action: &str,
    gaps: &[String],
    offer_local_only: bool,
) -> VogtError {
    let enumerated = gaps
        .iter()
        .enumerate()
        .map(|(index, gap)| format!("({}) {gap}", index + 1))
        .collect::<Vec<_>>()
        .join("; ");
    let tail = if offer_local_only {
        " Or pass `local_only` to create the item locally, without upstreaming it now."
    } else {
        ""
    };
    VogtError::UpstreamWriteRefused(format!(
        "cannot {action} on linked project {}: on a linked project the write goes upstream or not at all, and {} precondition(s) are unmet: {enumerated}.{tail}",
        crate::core::py_repr(&project.slug),
        gaps.len(),
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

/// Change a native item's fields. State changes go through `transition`.
///
/// A native item on an unlinked project refuses only when the edit touches
/// labels, which are shared vocabulary with the forge (decision 10); every
/// other field stays a local edit. An item that exists only upstream — the
/// forge mirror, not a declared row — is the write-through, which is not
/// ported and fails typed rather than writing an overlay the forge never
/// confirmed. The initiative re-projection that follows a committed update is
/// a forge write and is not done here.
fn update_work<C, I>(ctx: &AppContext<C, I>, params: Value) -> Result<Value, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let params = parse_update(params)?;
    let view = ctx.declared.read()?;
    // Declared ref first, then an upstream subject key, then not_found. A
    // typo'd `WI-n` never reaches the write-through: only a ref that resolves
    // to an observed item on a linked project does.
    upstream::resolve_work_ref(ctx, &view, &params.reference)?;
    let native = view.work_item_by_ref(&params.reference)?.is_some();
    if !native {
        return Err(VogtError::UpstreamWriteFailed(format!(
            "work.update of {} is an upstream item, and the write-through is not ported — nothing was stored locally",
            crate::core::py_repr(&params.reference),
        )));
    }
    let item = resolve::work_item(&view, &params.reference)?;
    if params.touches_labels() {
        if let Some(project_id) = item.project_id.as_deref() {
            if let Some(project) = view.project_by_id(project_id)? {
                if !upstream::is_linked(&project) {
                    return Err(refuse_unlinked_for(&project, "work.update with labels"));
                }
            }
        }
    }
    drop(view);
    update_native(ctx, &params)
}

struct UpdateParams {
    reference: String,
    title: Option<String>,
    body: Option<String>,
    priority: Option<String>,
    effort: Option<String>,
    project: Option<String>,
    initiative: Option<String>,
    assignee: Option<String>,
    clear_effort: bool,
    clear_assignee: bool,
    clear_initiative: bool,
    add_labels: Vec<String>,
    remove_labels: Vec<String>,
    reason: String,
}

impl UpdateParams {
    fn touches_labels(&self) -> bool {
        !self.add_labels.is_empty() || !self.remove_labels.is_empty()
    }
}

fn parse_update(params: Value) -> Result<UpdateParams, VogtError> {
    let Value::Object(map) = params else {
        return Err(VogtError::InvalidRequest(
            "work.update takes an object".to_string(),
        ));
    };
    let text = |key: &str| match map.get(key) {
        None | Some(Value::Null) => None,
        Some(Value::String(value)) => Some(value.clone()),
        Some(_) => Some(String::new()),
    };
    let reference = text("ref")
        .or_else(|| text("id"))
        .ok_or_else(|| VogtError::InvalidRequest("ref is required".into()))?;
    let names = |key: &str| {
        map.get(key)
            .and_then(Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    };
    let flag = |key: &str| map.get(key).and_then(Value::as_bool).unwrap_or(false);
    Ok(UpdateParams {
        reference,
        title: text("title"),
        body: text("body"),
        priority: text("priority"),
        effort: text("effort"),
        project: text("project"),
        initiative: text("initiative"),
        assignee: text("assignee"),
        clear_effort: flag("clear_effort"),
        clear_assignee: flag("clear_assignee"),
        clear_initiative: flag("clear_initiative"),
        add_labels: names("add_labels"),
        remove_labels: names("remove_labels"),
        reason: text("reason").unwrap_or_default(),
    })
}

fn update_native<C, I>(ctx: &AppContext<C, I>, params: &UpdateParams) -> Result<Value, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let mut writing = crate::application::context::write_of(ctx);
    let clock = std::sync::Arc::clone(writing.clock());
    let reference = params.reference.clone();
    let title = params.title.clone();
    let body = params.body.clone();
    let priority = params.priority.clone();
    let effort = params.effort.clone();
    let project = params.project.clone();
    let initiative = params.initiative.clone();
    let assignee = params.assignee.clone();
    let clear_effort = params.clear_effort;
    let clear_assignee = params.clear_assignee;
    let clear_initiative = params.clear_initiative;
    let add_labels = params.add_labels.clone();
    let remove_labels = params.remove_labels.clone();
    let stored = audited_write(
        &mut writing,
        WORK_UPDATE,
        &params.reason,
        move |txn, _actor| {
            let item = resolve::work_item(txn, &reference)?;
            for name in &add_labels {
                resolve::label_exists(txn, name)?;
            }
            let priority = match priority.as_deref() {
                Some(value) => Some(value.parse::<Priority>().map_err(|_| {
                    VogtError::InvalidRequest(
                        "priority must be one of p0, p1, p2, p3, p4".to_string(),
                    )
                })?),
                None => None,
            };
            let effort = match effort.as_deref() {
                Some(value) => Some(value.parse::<Effort>().map_err(|_| {
                    VogtError::InvalidRequest(
                        "effort must be one of trivial, small, medium, large".to_string(),
                    )
                })?),
                None => None,
            };
            let assignee_id = assignee
                .as_deref()
                .map(|identity| resolve::actor(txn, identity).map(|found| found.id))
                .transpose()?;
            let initiative_id = initiative
                .as_deref()
                .map(|slug| resolve::initiative(txn, slug).map(|found| found.id))
                .transpose()?;
            let project_id = project
                .as_deref()
                .map(|slug| resolve::project(txn, slug).map(|found| found.id))
                .transpose()?;
            let now = super::now_of(&clock);
            txn.update_work_item(
                &item.id,
                &WorkItemUpdate {
                    title,
                    body,
                    state: None,
                    priority: priority.map(|value| value.to_string()),
                    effort: effort.map(|value| value.to_string()),
                    assignee_actor_id: assignee_id,
                    initiative_id,
                    project_id,
                    clear_effort,
                    clear_assignee,
                    clear_initiative,
                    add_labels,
                    remove_labels,
                    superseded_by: None,
                },
                now,
            )?;
            let updated = txn
                .work_item_by_id(&item.id)?
                .ok_or_else(|| VogtError::NotFound(format!("no work item {}", item.reference)))?;
            let payload = serde_json::to_value(&updated).unwrap_or(Value::Null);
            Ok(WriteOutcome::new(
                payload.clone(),
                "work_item",
                &item.id,
                payload,
                WORK_UPDATED_EVENT,
                json!({"ref": item.reference}),
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
    let page_empty = page.is_empty();
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
    let mut page = serde_json::Map::new();
    page.insert("items".to_string(), Value::Array(items));
    page.insert("total".to_string(), json!(total));
    page.insert("mode".to_string(), Value::String(mode.to_string()));
    page.insert(
        "next_offset".to_string(),
        (!page_empty && following < total)
            .then_some(json!(following))
            .unwrap_or(Value::Null),
    );
    page.insert(
        "link_state".to_string(),
        link_state.map_or(Value::Null, |state| Value::String(state.to_string())),
    );
    page.insert("detail".to_string(), Value::Null);
    Value::Object(page)
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
    use super::{create_work, update_work, work_page};
    use crate::application::context::{build_context, Built};
    use crate::core::{
        ActorKind, LinkState, Moment, Principal, Project, SequentialIds, StepClock, WriteBack,
    };
    use crate::errors::VogtError;
    use crate::storage::interface::{DeclaredStore, ObservedStore, WriteTxn};
    use serde_json::{json, Value};

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
            std::env::temp_dir().join(format!("vogt-work-{}-{}", std::process::id(), unique()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let config = crate::config::VogtConfig {
            data_dir: dir,
            ..crate::config::VogtConfig::default()
        };
        let built = build_context(
            config,
            Some(Principal::new("local:test-user", ActorKind::Human, "Test").unwrap()),
            Some(StepClock::new(moment())),
            Some(SequentialIds::new(None).unwrap()),
            None,
            None,
            None,
            None,
        )
        .unwrap();
        let Built::StepSequential(ctx) = &built else {
            panic!("expected a step clock");
        };
        ctx.declared.migrate().unwrap();
        ctx.declared
            .bootstrap(&Principal::new("local:test-user", ActorKind::Human, "Test").unwrap())
            .unwrap();
        ctx.observed.migrate().unwrap();
        built
    }

    fn insert_project(built: &Built, slug: &str, linked: bool, policy: WriteBack) {
        let Built::StepSequential(ctx) = built else {
            unreachable!("opened() builds a step clock");
        };
        let mut project = Project::new(&format!("prj_{slug}"), slug, slug, "/tmp", moment());
        project.link_state = if linked {
            LinkState::Linked
        } else {
            LinkState::Unlinked
        };
        project.write_back = policy;
        project.repo_url = Some("https://github.com/acme/beta".to_string());
        let mut txn = ctx.declared.write().unwrap();
        txn.insert_project(&project).unwrap();
        txn.commit().unwrap();
    }

    #[test]
    fn the_envelope_carries_every_key_in_the_model_order() {
        // `WorkListResult`'s field order, with the nulls present. A global list
        // has no link state and an empty page has no next offset, and neither
        // is a reason to drop the key.
        let page = work_page("summary", 0, Vec::new(), 0, None);
        let Value::Object(map) = &page else {
            panic!("a page is an object");
        };
        let keys: Vec<&String> = map.keys().collect();
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
        let Value::Object(scoped_map) = &scoped else {
            panic!("a page is an object");
        };
        assert_eq!(scoped_map.keys().collect::<Vec<_>>(), keys);
        assert_eq!(scoped["link_state"], "unlinked");
    }

    #[test]
    fn a_native_create_keeps_the_body_and_numbers_refs_in_order() {
        let built = opened();
        let Built::StepSequential(ctx) = &built else {
            unreachable!("opened() builds a step clock");
        };
        let first = create_work(
            ctx,
            json!({"kind": "chore", "title": "one", "body": "  kept  ", "reason": "test"}),
        )
        .unwrap();
        assert_eq!(first["item"]["ref"], "WI-1");
        assert_eq!(first["item"]["body"], "  kept  ");
        assert_eq!(first["item"]["origin"], "created");
        assert_eq!(first["item"]["trust_state"], "unverified");
        let second = create_work(
            ctx,
            json!({"kind": "bug", "title": "two", "reason": "test"}),
        )
        .unwrap();
        assert_eq!(second["item"]["ref"], "WI-2");
        assert_eq!(second["item"]["state"], "open");
    }

    #[test]
    fn an_update_changes_the_title_and_keeps_the_ref() {
        let built = opened();
        let Built::StepSequential(ctx) = &built else {
            unreachable!("opened() builds a step clock");
        };
        create_work(
            ctx,
            json!({"kind": "chore", "title": "one", "reason": "test"}),
        )
        .unwrap();
        let updated = update_work(
            ctx,
            json!({"ref": "WI-1", "title": "renamed", "priority": "p0", "reason": "test"}),
        )
        .unwrap();
        assert_eq!(updated["item"]["ref"], "WI-1");
        assert_eq!(updated["item"]["title"], "renamed");
        assert_eq!(updated["item"]["priority"], "p0");
        assert_eq!(updated["item"]["kind"], "chore");
    }

    #[test]
    fn a_label_edit_on_an_unlinked_project_is_refused() {
        let built = opened();
        let Built::StepSequential(ctx) = &built else {
            unreachable!("opened() builds a step clock");
        };
        insert_project(&built, "beta", false, WriteBack::Disabled);
        create_work(
            ctx,
            json!({"kind": "chore", "title": "one", "project": "beta", "local_only": true, "reason": "test"}),
        )
        .unwrap();
        let refused = update_work(
            ctx,
            json!({"ref": "WI-1", "add_labels": ["parity"], "reason": "test"}),
        )
        .unwrap_err();
        assert!(matches!(refused, VogtError::NotLinked(_)), "{refused}");
        let retitled = update_work(
            ctx,
            json!({"ref": "WI-1", "title": "still editable", "reason": "test"}),
        )
        .unwrap();
        assert_eq!(retitled["item"]["title"], "still editable");
    }

    #[test]
    fn an_unknown_ref_is_not_found_rather_than_an_upstream_item() {
        let built = opened();
        let Built::StepSequential(ctx) = &built else {
            unreachable!("opened() builds a step clock");
        };
        for reference in ["WI-99", "wi-2"] {
            let refused = update_work(
                ctx,
                json!({"ref": reference, "title": "nope", "reason": "test"}),
            )
            .unwrap_err();
            let VogtError::NotFound(message) = &refused else {
                panic!("{reference}: expected not_found, got {refused}");
            };
            assert!(message.contains("no work item"), "{reference}: {message}");
        }
    }

    #[test]
    fn an_unlinked_project_refuses_without_burning_a_ref() {
        let built = opened();
        let Built::StepSequential(ctx) = &built else {
            unreachable!("opened() builds a step clock");
        };
        insert_project(&built, "beta", false, WriteBack::Disabled);
        let refused = create_work(
            ctx,
            json!({"kind": "chore", "title": "nope", "project": "beta", "reason": "test"}),
        )
        .unwrap_err();
        assert!(matches!(refused, VogtError::NotLinked(_)), "{refused}");
        assert!(
            refused.message().contains("local_only: true"),
            "{}",
            refused.message()
        );
        let after = create_work(
            ctx,
            json!({"kind": "chore", "title": "still first", "reason": "test"}),
        )
        .unwrap();
        assert_eq!(after["item"]["ref"], "WI-1");
    }

    #[test]
    fn a_linked_project_names_every_unmet_precondition() {
        let built = opened();
        let Built::StepSequential(ctx) = &built else {
            unreachable!("opened() builds a step clock");
        };
        insert_project(&built, "beta", true, WriteBack::Disabled);
        let refused = create_work(
            ctx,
            json!({"kind": "chore", "title": "nope", "project": "beta", "reason": "test"}),
        )
        .unwrap_err();
        let VogtError::UpstreamWriteRefused(message) = &refused else {
            panic!("expected upstream_write_refused, got {refused}");
        };
        assert!(
            message.contains("write-back policy is 'none'")
                && message.contains("(2) no forge credential"),
            "{message}"
        );
        let after = create_work(
            ctx,
            json!({"kind": "chore", "title": "still first", "reason": "test"}),
        )
        .unwrap();
        assert_eq!(after["item"]["ref"], "WI-1");
    }
}
