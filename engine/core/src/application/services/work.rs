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
//! write-through: the issue is opened upstream first and the overlay is stored
//! only once the forge accepted it. An unlinked project without `local_only`
//! is the decision-10 refusal.

use serde_json::{json, Value};

use crate::adapters::forge::registry::ForgeClient;
use crate::adapters::forge::{ForgeProvider, RepoRef, WriteBackOutcome, WriteBackResult};
use crate::application::context::{AppContext, Built};
use crate::application::resolve;
use crate::application::services::forge_writeback::writer_provider;
use crate::application::upstream;
use crate::application::writes::{audited_write, WriteOutcome};
use crate::core::{
    Clock, Comment, Effort, IdFactory, Origin, Priority, Project, TrustState, WorkItem, WorkKind,
    WorkLink, WorkOverlay, WriteBackRecord, TERMINAL_STATES,
};
use crate::errors::VogtError;
use crate::storage::interface::{
    DeclaredStore, ObservedStore, ReadView, WorkFilter, WorkItemUpdate, WriteTxn,
};

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

pub fn get_op(ctx: &Built, params: Value) -> Result<Value, VogtError> {
    crate::with_ctx!(ctx, |ctx| get_work(ctx, params))
}

pub fn get_many_op(ctx: &Built, params: Value) -> Result<Value, VogtError> {
    crate::with_ctx!(ctx, |ctx| get_many_work(ctx, params))
}

pub fn comment_op(ctx: &Built, params: Value) -> Result<Value, VogtError> {
    crate::with_ctx!(ctx, |ctx| comment_work(ctx, params))
}

pub fn transition_op(ctx: &Built, params: Value) -> Result<Value, VogtError> {
    crate::with_ctx!(ctx, |ctx| transition_work(ctx, params))
}

pub fn relate_op(ctx: &Built, params: Value) -> Result<Value, VogtError> {
    crate::with_ctx!(ctx, |ctx| relate_work(ctx, params))
}

pub fn unrelate_op(ctx: &Built, params: Value) -> Result<Value, VogtError> {
    crate::with_ctx!(ctx, |ctx| unrelate_work(ctx, params))
}

pub fn bind_branch_op(ctx: &Built, params: Value) -> Result<Value, VogtError> {
    crate::with_ctx!(ctx, |ctx| bind_branch(ctx, params))
}

pub fn adopt_op(ctx: &Built, params: Value) -> Result<Value, VogtError> {
    crate::with_ctx!(ctx, |ctx| adopt_work(ctx, params))
}

pub fn board_list_op(ctx: &Built, params: Value) -> Result<Value, VogtError> {
    crate::with_ctx!(ctx, |ctx| list_board(ctx, params))
}

/// Create a work item. No project, or `local_only`, writes a native declared
/// item. A linked project without `local_only` is the forge write-through
/// (`_create_upstream`). An unlinked project without `local_only` is the
/// decision-10 refusal.
pub(crate) fn create_work<C, I>(ctx: &AppContext<C, I>, params: Value) -> Result<Value, VogtError>
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
    create_upstream(ctx, &params, &project)
}

/// Write-through create: the issue first, the overlay only on success. Ports
/// `_create_upstream`.
///
/// Decision 9: the provider call runs before the declared transaction opens,
/// so a network round trip never holds the write lock, and a refusal or
/// failure returns before anything local exists. The crash window left (issue
/// created, local transaction lost) heals on the next sweep.
fn create_upstream<C, I>(
    ctx: &AppContext<C, I>,
    params: &CreateParams,
    project: &Project,
) -> Result<Value, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    require_permitted(ctx, project, "create", true)?;
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
    let labels = params.labels.clone();
    let (workflow, initiative_id, assignee_id) = {
        let view = ctx.declared.read()?;
        let workflow = view.workflow_for(&kind.to_string())?;
        let initiative_id = params
            .initiative
            .as_deref()
            .map(|slug| resolve::initiative(&view, slug).map(|found| found.id))
            .transpose()?;
        let assignee_id = params
            .assignee
            .as_deref()
            .map(|identity| resolve::actor(&view, identity).map(|found| found.id))
            .transpose()?;
        for name in &labels {
            resolve::label_exists(&view, name)?;
        }
        (workflow, initiative_id, assignee_id)
    };

    let (provider, identity, repo) = upstream_writer(ctx, project)?;
    let sent = sent_or_fail(
        provider.create_issue(
            &repo,
            &params.title,
            &params.body,
            (!labels.is_empty()).then_some(labels.as_slice()),
        )?,
        "issue create",
    )?;
    let Some(subject_key) = sent.subject_key.clone().filter(|key| !key.is_empty()) else {
        return Err(VogtError::UpstreamWriteFailed(
            "the forge accepted the issue but returned no number, so there is no subject key to track it under — nothing was stored locally".to_string(),
        ));
    };

    let mut writing = crate::application::context::write_of(ctx);
    let clock = std::sync::Arc::clone(writing.clock());
    let ids = std::sync::Arc::clone(writing.ids());
    let project = project.clone();
    let title = params.title.clone();
    let body = params.body.clone();
    let reason = params.reason.clone();
    let stored = audited_write(
        &mut writing,
        WORK_CREATE,
        &params.reason,
        move |txn, actor| {
            let now = super::now_of(&clock);
            let mut overlay =
                fresh_overlay(txn.work_overlay(&subject_key)?, &subject_key, &project, now);
            overlay.workflow_state = Some(workflow.initial_state.clone());
            overlay.priority = Some(priority);
            overlay.effort = effort;
            overlay.assignee_actor_id = assignee_id.clone();
            overlay.initiative_id = initiative_id.clone();
            txn.upsert_work_overlay(&overlay)?;
            txn.insert_writeback(&upstream_record(
                &ids,
                &clock,
                &actor.id,
                crate::core::WriteBackAction::Create,
                &project,
                Some(&subject_key),
                &reason,
                &identity,
                &sent,
            ))?;
            // The mirror learns about the issue on the next sweep; until then the
            // item is assembled from what was just sent.
            let item = WorkItem {
                id: subject_key.clone(),
                reference: subject_key.clone(),
                kind,
                title: title.clone(),
                body: body.clone(),
                state: workflow.initial_state.clone(),
                priority,
                effort,
                project_id: Some(project.id.clone()),
                project_slug: Some(project.slug.clone()),
                initiative_id: initiative_id.clone(),
                origin: Origin::Observed,
                trust_state: TrustState::Unverified,
                assignee_actor_id: assignee_id.clone(),
                assignee_identity_ref: None,
                labels: labels.clone(),
                relations: Vec::new(),
                superseded_by: None,
                created_at: now,
                updated_at: now,
            };
            let payload = serde_json::to_value(&item).unwrap_or(Value::Null);
            let summary = json!({"ref": subject_key, "kind": kind, "title": title});
            Ok(WriteOutcome::new(
                item,
                "work_item",
                &subject_key,
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
/// project's `repo_url` parses for that provider. Credentials resolve from
/// local configuration (`writer_provider`); nothing here touches the network.
fn write_through_gaps<C, I>(
    ctx: &AppContext<C, I>,
    project: &Project,
    action: &str,
) -> Result<Vec<String>, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let gaps = policy_gap(project, action);
    let actor = ctx
        .declared
        .read()?
        .actor_by_identity(&ctx.principal.identity_ref)?;
    gaps_for(ctx, project, actor.as_ref(), gaps)
}

/// `write_through_gaps` with the acting actor already resolved, after the
/// policy gate. Ports the credential and parse half of `_gaps_for`.
fn gaps_for<C, I>(
    ctx: &AppContext<C, I>,
    project: &Project,
    actor: Option<&crate::core::Actor>,
    mut gaps: Vec<String>,
) -> Result<Vec<String>, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let (provider, _identity) = writer_provider(ctx, actor, project.repo_url.as_deref())?;
    match provider {
        None => gaps.push(
            "no forge credential resolves for this project — link your forge account with `forge account link`, or configure the instance token file".to_string(),
        ),
        Some(provider) if provider.parse(project.repo_url.as_deref()).is_none() => {
            gaps.push(format!(
                "the project's repo_url {} does not parse for the forge provider — correct it with `project update`",
                py_repr_option(project.repo_url.as_deref()),
            ))
        }
        Some(_) => {}
    }
    Ok(gaps)
}

/// Would a default `work.create` land in `project` now, and why. Ports
/// `create_writability`: the decision-10 link check, then every write-through
/// precondition, evaluated without writing or touching the network.
pub(crate) fn create_writability<C, I>(
    ctx: &AppContext<C, I>,
    project: &Project,
    actor: Option<&crate::core::Actor>,
) -> Result<(bool, String), VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    if !upstream::is_linked(project) {
        return Ok((
            false,
            "not forge-linked: work.create refuses with project_not_linked. Pass local_only=true for a local record, or link (`forge link`) or publish (`forge publish`) the project.".to_string(),
        ));
    }
    let gaps = gaps_for(ctx, project, actor, policy_gap(project, "create"))?;
    if gaps.is_empty() {
        return Ok((
            true,
            "linked and write-back permits create: work.create opens the issue upstream."
                .to_string(),
        ));
    }
    Ok((
        false,
        format!(
            "linked, but work.create refuses with upstream_write_refused: {}. Or pass local_only=true for a local record.",
            gaps.join("; ")
        ),
    ))
}

/// The policy gate of `_gaps_for`: empty when the policy permits `action`.
fn policy_gap(project: &Project, action: &str) -> Vec<String> {
    let policy = project.write_back.to_string();
    if crate::adapters::forge::permits(&policy, action) {
        return Vec::new();
    }
    vec![format!(
        "write-back policy is {}, which does not permit {} — set it with `forge writeback`",
        crate::core::py_repr(&policy),
        crate::core::py_repr(action),
    )]
}

/// Python's `repr` of an optional string: `None` or the quoted text.
fn py_repr_option(value: Option<&str>) -> String {
    value.map_or_else(|| "None".to_string(), crate::core::py_repr)
}

/// Refuse a write-through whose chain is incomplete, naming every gap. Ports
/// `_require_permitted`.
fn require_permitted<C, I>(
    ctx: &AppContext<C, I>,
    project: &Project,
    action: &str,
    offer_local_only: bool,
) -> Result<(), VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let gaps = write_through_gaps(ctx, project, action)?;
    if gaps.is_empty() {
        return Ok(());
    }
    Err(refuse_gaps(project, action, &gaps, offer_local_only))
}

/// The provider, identity and repo a write-through will use, resolved before
/// the declared transaction opens. Ports `_upstream_writer`.
fn upstream_writer<C, I>(
    ctx: &AppContext<C, I>,
    project: &Project,
) -> Result<(ForgeClient, String, RepoRef), VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let actor = ctx
        .declared
        .read()?
        .actor_by_identity(&ctx.principal.identity_ref)?;
    let (provider, identity) = writer_provider(ctx, actor.as_ref(), project.repo_url.as_deref())?;
    let Some(provider) = provider else {
        return Err(VogtError::UpstreamWriteFailed(format!(
            "{} is linked but no usable forge credential resolves for it now — link a forge account or restore the instance token file, then retry",
            crate::core::py_repr(&project.slug)
        )));
    };
    let Some(repo) = provider.parse(project.repo_url.as_deref()) else {
        return Err(VogtError::UpstreamWriteFailed(format!(
            "{} is linked but its repo_url {} no longer parses for the provider",
            crate::core::py_repr(&project.slug),
            py_repr_option(project.repo_url.as_deref()),
        )));
    };
    Ok((provider, identity, repo))
}

/// Decision 9: anything but an upstream success aborts the operation.
fn sent_or_fail(result: WriteBackResult, action: &str) -> Result<WriteBackResult, VogtError> {
    if result.outcome != WriteBackOutcome::Succeeded {
        return Err(VogtError::UpstreamWriteFailed(format!(
            "the forge did not accept the {action}: {} — nothing was changed locally",
            result
                .detail
                .as_deref()
                .filter(|detail| !detail.is_empty())
                .unwrap_or("no detail from the provider"),
        )));
    }
    Ok(result)
}

/// The write-back ledger row for one successful write-through. Ports
/// `_upstream_record`.
#[allow(clippy::too_many_arguments)]
fn upstream_record(
    ids: &std::sync::Arc<std::sync::Mutex<impl IdFactory>>,
    clock: &std::sync::Arc<std::sync::Mutex<impl Clock>>,
    actor_id: &str,
    action: crate::core::WriteBackAction,
    project: &Project,
    subject_key: Option<&str>,
    reason: &str,
    identity: &str,
    sent: &WriteBackResult,
) -> WriteBackRecord {
    WriteBackRecord {
        id: super::next_id(ids, "wbk"),
        at: super::now_of(clock),
        project_id: Some(project.id.clone()),
        work_item_id: None,
        actor_id: actor_id.to_string(),
        action,
        subject_key: sent
            .subject_key
            .clone()
            .filter(|key| !key.is_empty())
            .or_else(|| subject_key.map(str::to_string)),
        policy: project.write_back.to_string(),
        outcome: crate::core::WriteBackOutcome::Succeeded,
        reason: reason.to_string(),
        detail: Some(format!("as {identity}")),
        source_url: sent.source_url.clone(),
    }
}

/// The overlay row to build on: the existing one, or an empty one for this
/// subject. The caller sets the changed fields; `updated_at` is `now`.
fn fresh_overlay(
    existing: Option<WorkOverlay>,
    subject_key: &str,
    project: &Project,
    now: crate::core::Moment,
) -> WorkOverlay {
    let mut overlay = existing.unwrap_or_else(|| WorkOverlay {
        subject_key: subject_key.to_string(),
        project_id: project.id.clone(),
        rank: None,
        workflow_state: None,
        priority: None,
        effort: None,
        assignee_actor_id: None,
        initiative_id: None,
        branches: Vec::new(),
        created_at: now,
        updated_at: now,
    });
    overlay.updated_at = now;
    overlay
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
/// forge mirror, not a declared row — goes through `update_upstream`. The
/// initiative re-projection that follows a committed update is
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
    let resolved = upstream::resolve_work_ref(ctx, &view, &params.reference)?;
    let native = view.work_item_by_ref(&params.reference)?.is_some();
    if !native {
        let project_id = resolved
            .project_id
            .clone()
            .ok_or_else(|| VogtError::NotFound(format!("{} has no project", resolved.reference)))?;
        let project = view
            .project_by_id(&project_id)?
            .ok_or_else(|| VogtError::NotFound(format!("no project {project_id}")))?;
        drop(view);
        return update_upstream(ctx, &params, resolved, &project);
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

/// Split an upstream item's update along the boundary decision 2 draws. Ports
/// `_update_upstream`.
///
/// Labels are shared vocabulary: an upstream `add_labels` write, fail-loud.
/// Title and body are upstream truth vogt does not edit, an item cannot move
/// project, and the write surface never removes a label. Priority, effort,
/// assignee and initiative are vogt-local: overlay only, no provider call.
fn update_upstream<C, I>(
    ctx: &AppContext<C, I>,
    params: &UpdateParams,
    item: WorkItem,
    project: &Project,
) -> Result<Value, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    if params.title.is_some() || params.body.is_some() {
        return Err(VogtError::InvalidRequest(format!(
            "{} is upstream truth: vogt does not edit issue titles or bodies on a linked project (the write-back verb set has no edit) — edit it on the forge",
            item.reference
        )));
    }
    if params.project.is_some() {
        return Err(VogtError::InvalidRequest(format!(
            "{} lives in {}'s repository; an upstream issue cannot be moved to another project",
            item.reference,
            crate::core::py_repr(&project.slug)
        )));
    }
    if !params.remove_labels.is_empty() {
        return Err(VogtError::InvalidRequest(format!(
            "{} is upstream truth and the write surface is append-only: labels can be added upstream, never removed from here — remove them on the forge",
            item.reference
        )));
    }
    let priority = params
        .priority
        .as_deref()
        .map(|value| {
            value.parse::<Priority>().map_err(|_| {
                VogtError::InvalidRequest("priority must be one of p0, p1, p2, p3, p4".to_string())
            })
        })
        .transpose()?;
    let effort = params
        .effort
        .as_deref()
        .map(|value| {
            value.parse::<Effort>().map_err(|_| {
                VogtError::InvalidRequest(format!("unknown effort {}", crate::core::py_repr(value)))
            })
        })
        .transpose()?;
    let add_labels = params.add_labels.clone();
    let (assignee_id, initiative_id) = {
        let view = ctx.declared.read()?;
        for name in &add_labels {
            resolve::label_exists(&view, name)?;
        }
        let assignee_id = params
            .assignee
            .as_deref()
            .map(|identity| resolve::actor(&view, identity).map(|found| found.id))
            .transpose()?;
        let initiative_id = params
            .initiative
            .as_deref()
            .map(|slug| resolve::initiative(&view, slug).map(|found| found.id))
            .transpose()?;
        (assignee_id, initiative_id)
    };

    let mut sent = None;
    let mut identity = String::new();
    if !add_labels.is_empty() {
        require_permitted(ctx, project, "label", false)?;
        let (provider, who, repo) = upstream_writer(ctx, project)?;
        let number = provider.number_of(Some(&item.reference)).ok_or_else(|| {
            VogtError::UpstreamWriteFailed(format!(
                "{} carries no issue number to label upstream",
                item.reference
            ))
        })?;
        sent = Some(sent_or_fail(
            provider.add_labels(&repo, number, &add_labels)?,
            "label add",
        )?);
        identity = who;
    }

    let mut writing = crate::application::context::write_of(ctx);
    let clock = std::sync::Arc::clone(writing.clock());
    let ids = std::sync::Arc::clone(writing.ids());
    let project = project.clone();
    let reason = params.reason.clone();
    let clear_effort = params.clear_effort;
    let clear_assignee = params.clear_assignee;
    let clear_initiative = params.clear_initiative;
    let stored = audited_write(
        &mut writing,
        WORK_UPDATE,
        &params.reason,
        move |txn, actor| {
            let now = super::now_of(&clock);
            let mut overlay = fresh_overlay(
                txn.work_overlay(&item.reference)?,
                &item.reference,
                &project,
                now,
            );
            let mut updated = item.clone();
            if let Some(priority) = priority {
                overlay.priority = Some(priority);
                updated.priority = priority;
            }
            if effort.is_some() || clear_effort {
                overlay.effort = effort;
                updated.effort = effort;
            }
            if assignee_id.is_some() || clear_assignee {
                overlay.assignee_actor_id = assignee_id.clone();
                updated.assignee_actor_id = assignee_id.clone();
            }
            if initiative_id.is_some() || clear_initiative {
                overlay.initiative_id = initiative_id.clone();
                updated.initiative_id = initiative_id.clone();
            }
            txn.upsert_work_overlay(&overlay)?;
            if let Some(sent) = &sent {
                txn.insert_writeback(&upstream_record(
                    &ids,
                    &clock,
                    &actor.id,
                    crate::core::WriteBackAction::Label,
                    &project,
                    Some(&item.reference),
                    &reason,
                    &identity,
                    sent,
                ))?;
            }
            for label in &add_labels {
                if !updated.labels.contains(label) {
                    updated.labels.push(label.clone());
                }
            }
            updated.updated_at = now;
            let payload = serde_json::to_value(&updated).unwrap_or(Value::Null);
            Ok(WriteOutcome::new(
                updated,
                "work_item",
                &item.reference,
                payload,
                WORK_UPDATED_EVENT,
                json!({"ref": item.reference}),
            ))
        },
    )?;
    // Python re-projects initiative membership after the commit; that is a
    // forge write the native path does not do here either.
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
                summary_row(item)
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

/// How many refs one `work.get_many` may name. Python's `WORK_GET_MANY_LIMIT`.
const WORK_GET_MANY_LIMIT: usize = 200;

/// The compact row a summary listing and `work.get_many` both return: what a
/// reader scans, nothing more. Python's `WorkItemRow`.
fn summary_row(item: &WorkItem) -> Value {
    json!({
        "ref": item.reference,
        "title": item.title,
        "kind": item.kind.to_string(),
        "state": item.state,
        "priority": item.priority.to_string(),
        "project_slug": item.project_slug,
    })
}

/// Compact rows for a set of refs, in one read: what the session rail's chips
/// ask so a render costs one request rather than one `work.list` per ref
/// (WI-1224). Each ref resolves as `work.get` resolves it; a repeated ref is
/// read once, a ref that names nothing is reported in `missing`, and the rest
/// still come back. Python's `get_many_work`.
fn get_many_work<C: Clock, I: IdFactory>(
    ctx: &AppContext<C, I>,
    params: Value,
) -> Result<Value, VogtError> {
    let refs = strings(params.get("refs"));
    if refs.is_empty() || refs.len() > WORK_GET_MANY_LIMIT {
        return Err(VogtError::InvalidRequest(format!(
            "work.get_many takes 1 to {WORK_GET_MANY_LIMIT} refs"
        )));
    }
    if refs
        .iter()
        .any(|reference| reference.is_empty() || reference.len() > 200)
    {
        return Err(VogtError::InvalidRequest(
            "each ref is 1-200 characters".into(),
        ));
    }
    let view = ctx.declared.read()?;
    let mut items = Vec::new();
    let mut missing = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for reference in refs {
        if !seen.insert(reference.clone()) {
            continue;
        }
        match upstream::resolve_work_ref(ctx, &view, &reference) {
            Ok(item) => items.push(summary_row(&item)),
            Err(VogtError::NotFound(_)) => missing.push(reference),
            Err(other) => return Err(other),
        }
    }
    Ok(json!({"items": items, "missing": missing}))
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

/// One work item with its comments, sessions, branches and git story.
///
/// A declared `WI-n` and an upstream subject key resolve the same way. Comments
/// and sessions belong to a declared row; an upstream item has none of either
/// until it is adopted. Branches and the git story are derived reads and come
/// back for either.
fn get_work<C: Clock, I: IdFactory>(
    ctx: &AppContext<C, I>,
    params: Value,
) -> Result<Value, VogtError> {
    let reference = params
        .get("ref")
        .or_else(|| params.get("id"))
        .and_then(Value::as_str)
        .ok_or_else(|| VogtError::InvalidRequest("ref is required".into()))?;
    let limit = params
        .get("comment_limit")
        .and_then(Value::as_i64)
        .unwrap_or(200);
    let newest = params.get("comment_order").and_then(Value::as_str) == Some("newest");
    let summary = params.get("mode").and_then(Value::as_str) == Some("summary");
    let view = ctx.declared.read()?;
    let mut item = upstream::resolve_work_ref(ctx, &view, reference)?;
    let native = view.work_item_by_ref(reference)?.is_some();
    let mut comments = if native && limit > 0 && !item.id.is_empty() {
        view.comments_for(&item.id, limit, newest)?
    } else {
        Vec::new()
    };
    if summary {
        item.body.clear();
        comments.clear();
    }
    // Branches, then the git story, then the session join: the order Python's
    // `get_work` reads the clock in, which the next timestamp depends on.
    let branches = crate::application::git_story::branch_views_for(ctx, &view, &item)?;
    let git = crate::application::git_story::git_story_for(ctx, &item, &branches)?;
    let sessions = sessions_for(ctx, &view, &item)?;
    Ok(json!({
        "item": item,
        "comments": comments,
        "sessions": sessions,
        "branches": branches,
        "git": git,
        "walked": [],
        "live_sessions": [],
    }))
}

/// Sessions bound to a declared item. An upstream item has no declared id, so
/// it has no sessions; the live engine half is not joined here.
fn sessions_for<C: Clock, I: IdFactory>(
    ctx: &AppContext<C, I>,
    view: &impl crate::storage::interface::ReadView,
    item: &WorkItem,
) -> Result<Vec<Value>, VogtError> {
    if item.id.is_empty() {
        return Ok(Vec::new());
    }
    // Python's session list reads the clock once for the attention of the
    // linked sessions, whether or not the engine answers. Nothing here uses the
    // value yet, but the tick is spent so the next timestamp matches.
    let _now = super::now_of(&ctx.clock);
    let rows = view.list_sessions(None, Some(&item.id), true, 200, 0)?;
    let mut out = Vec::new();
    for session in rows {
        let actor = view.actor_by_id(&session.actor_id)?;
        let project = view.project_by_id(&session.project_id)?;
        out.push(json!({
            "id": session.id,
            "engine_session_id": session.engine_session_id,
            "project": project.as_ref().map(|project| &project.slug),
            "work_item": item.reference,
            "work_item_title": item.title,
            "work_item_state": item.state,
            "actor": actor.as_ref().map(|actor| &actor.identity_ref),
            "cwd": session.cwd,
            "template": session.template,
            "model": session.model,
            "effort": session.effort,
            "reason": session.reason,
            "started_at": session.started_at,
            "stopped_at": session.stopped_at,
            "running": session.stopped_at.is_none() && ctx.engine.is_some(),
            "engine_available": ctx.engine.is_some(),
        }));
    }
    Ok(out)
}

const WORK_COMMENT: &str = "work.comment";
const WORK_COMMENTED: &str = "work.commented";
const WORK_TRANSITION: &str = "work.transition";
const WORK_TRANSITIONED: &str = "work.transitioned";
const WORK_RELATE: &str = "work.relate";
const WORK_RELATED: &str = "work.related";
const WORK_UNRELATE: &str = "work.unrelate";
const WORK_UNRELATED: &str = "work.unrelated";
const WORK_BIND: &str = "work.bind_branch";
const WORK_BRANCH_BOUND: &str = "work.branch_bound";
const WORK_ADOPT: &str = "work.adopt";
const WORK_ADOPTED: &str = "work.adopted";

/// Append a comment to a native item. `kind` is `note` or `handoff`; a
/// hand-off stores its structured fields. The write-back record is written
/// in the same transaction (`writeback.attempt`), outcome `skipped` while no
/// forge provider is wired. An upstream item is the write-through and stores
/// nothing.
fn comment_work<C, I>(ctx: &AppContext<C, I>, params: Value) -> Result<Value, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let reference = text_of(&params, "ref")
        .ok_or_else(|| VogtError::InvalidRequest("ref is required".into()))?;
    let body = text_of(&params, "body")
        .ok_or_else(|| VogtError::InvalidRequest("body is required".into()))?;
    let reason = text_of(&params, "reason").unwrap_or_default();
    let kind = text_of(&params, "kind").unwrap_or_else(|| "note".to_string());
    if kind != "note" && kind != "handoff" {
        return Err(VogtError::InvalidRequest(format!(
            "unknown comment kind {}",
            crate::core::py_repr(&kind)
        )));
    }
    if kind == "note"
        && params
            .get("handoff")
            .map(|value| !value.is_null())
            .unwrap_or(false)
    {
        return Err(VogtError::InvalidRequest(
            "handoff fields belong on a handoff comment".into(),
        ));
    }
    let handoff = params
        .get("handoff")
        .filter(|value| !value.is_null())
        .cloned();
    let view = ctx.declared.read()?;
    upstream::resolve_work_ref(ctx, &view, &reference)?;
    if view.work_item_by_ref(&reference)?.is_none() {
        return Err(VogtError::UpstreamWriteFailed(format!(
            "work.comment on {} is an upstream item, and the write-through is not ported — nothing was stored locally",
            crate::core::py_repr(&reference),
        )));
    }
    drop(view);
    let mut writing = crate::application::context::write_of(ctx);
    let clock = std::sync::Arc::clone(writing.clock());
    let ids = std::sync::Arc::clone(writing.ids());
    let clock_for_record = std::sync::Arc::clone(&clock);
    let ids_for_record = std::sync::Arc::clone(&ids);
    let reason_for_record = reason.clone();
    let (stored, outcome) =
        audited_write(&mut writing, WORK_COMMENT, &reason, move |txn, actor| {
            let item = resolve::work_item(txn, &reference)?;
            let now = super::now_of(&clock);
            let comment = Comment {
                id: super::next_id(&ids, "cmt"),
                work_item_id: item.id.clone(),
                actor_id: actor.id.clone(),
                actor_display_name: actor.display_name.clone(),
                body: body.clone(),
                created_at: now,
                kind: kind.clone(),
                handoff: handoff.clone(),
            };
            txn.insert_comment(&comment)?;
            let project = item
                .project_id
                .as_deref()
                .map(|id| txn.project_by_id(id))
                .transpose()?
                .flatten();
            let links = txn.work_links_for_subjects_by_item(&item.id)?;
            let subject = links.keys().next().cloned();
            let record = skipped_writeback(
                txn,
                &ids_for_record,
                &clock_for_record,
                &actor.id,
                crate::core::WriteBackAction::Comment,
                project.as_ref(),
                Some(&item.id),
                subject.as_deref(),
                &reason_for_record,
            )?;
            let outcome = record.outcome.to_string();
            let comment_id = comment.id.clone();
            let payload = serde_json::to_value(&comment).unwrap_or(Value::Null);
            Ok(WriteOutcome::new(
                (comment, outcome),
                "comment",
                &comment_id,
                payload,
                WORK_COMMENTED,
                json!({"ref": item.reference, "comment_id": comment_id}),
            ))
        })?;
    Ok(json!({
        "comment": stored,
        "write_back": outcome,
    }))
}

/// Move a native item along its workflow. `walk` takes the shortest legal path
/// one audited hop at a time. Completion is refused while a `depends_on`
/// target is unfinished. An upstream item is the write-through and stores
/// nothing.
fn transition_work<C, I>(ctx: &AppContext<C, I>, params: Value) -> Result<Value, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let reference = text_of(&params, "ref")
        .ok_or_else(|| VogtError::InvalidRequest("ref is required".into()))?;
    let to_state = text_of(&params, "to_state")
        .ok_or_else(|| VogtError::InvalidRequest("to_state is required".into()))?;
    let walk = params.get("walk").and_then(Value::as_bool).unwrap_or(false);
    let reason = text_of(&params, "reason").unwrap_or_default();
    let view = ctx.declared.read()?;
    upstream::resolve_work_ref(ctx, &view, &reference)?;
    if view.work_item_by_ref(&reference)?.is_none() {
        return Err(VogtError::UpstreamWriteFailed(format!(
            "work.transition of {} is an upstream item, and the write-through is not ported — nothing was stored locally",
            crate::core::py_repr(&reference),
        )));
    }
    let item = resolve::work_item(&view, &reference)?;
    let workflow = view.workflow_for(&item.kind.to_string())?;
    let path = if walk {
        workflow
            .shortest_path(&item.state, &to_state)
            .ok_or_else(|| {
                VogtError::InvalidRequest(format!("no path from {} to {to_state}", item.state))
            })?
    } else {
        workflow.check(&item.state, &to_state)?;
        vec![item.state.clone(), to_state.clone()]
    };
    if path.len() >= 2 && crate::core::TERMINAL_STATES.contains(&path.last().unwrap().as_str()) {
        let blockers = view.unfinished_blockers(&item.id, crate::core::TERMINAL_STATES)?;
        if !blockers.is_empty() {
            let names = blockers
                .iter()
                .map(|blocker| format!("{} ({})", blocker.r#ref, blocker.state))
                .collect::<Vec<_>>()
                .join(", ");
            return Err(crate::core::transition_rejected(
                "transition.blocked",
                &format!(
                    "{} is blocked by unfinished dependencies: {names}",
                    item.reference
                ),
            ));
        }
    }
    let hops = path.len().saturating_sub(1);
    drop(view);
    let mut current = item;
    for (number, window) in path.windows(2).enumerate() {
        let from = window[0].clone();
        let to = window[1].clone();
        let hop_reason = if walk {
            format!("{reason} (walk {}/{hops}: {from} -> {to})", number + 1)
        } else {
            reason.clone()
        };
        let mut writing = crate::application::context::write_of(ctx);
        let clock = std::sync::Arc::clone(writing.clock());
        let ids = std::sync::Arc::clone(writing.ids());
        let id = current.id.clone();
        let reference = current.reference.clone();
        let entering = crate::core::TERMINAL_STATES.contains(&to.as_str())
            && !crate::core::TERMINAL_STATES.contains(&from.as_str());
        let leaving = crate::core::TERMINAL_STATES.contains(&from.as_str())
            && !crate::core::TERMINAL_STATES.contains(&to.as_str());
        let hop_for_record = hop_reason.clone();
        current = audited_write(
            &mut writing,
            WORK_TRANSITION,
            &hop_reason,
            move |txn, actor| {
                let fresh = txn
                    .work_item_by_id(&id)?
                    .ok_or_else(|| VogtError::NotFound(format!("no work item {reference}")))?;
                let workflow = txn.workflow_for(&fresh.kind.to_string())?;
                workflow.check(&fresh.state, &to)?;
                if crate::core::TERMINAL_STATES.contains(&to.as_str()) {
                    let blockers =
                        txn.unfinished_blockers(&fresh.id, crate::core::TERMINAL_STATES)?;
                    if !blockers.is_empty() {
                        let names = blockers
                            .iter()
                            .map(|blocker| format!("{} ({})", blocker.r#ref, blocker.state))
                            .collect::<Vec<_>>()
                            .join(", ");
                        return Err(crate::core::transition_rejected(
                            "transition.blocked",
                            &format!("{reference} is blocked by unfinished dependencies: {names}"),
                        ));
                    }
                }
                let now = super::now_of(&clock);
                txn.update_work_item(
                    &fresh.id,
                    &WorkItemUpdate {
                        state: Some(to.clone()),
                        ..WorkItemUpdate::default()
                    },
                    now,
                )?;
                let updated = txn
                    .work_item_by_id(&fresh.id)?
                    .ok_or_else(|| VogtError::NotFound(format!("no work item {reference}")))?;
                let project = updated
                    .project_id
                    .as_deref()
                    .map(|id| txn.project_by_id(id))
                    .transpose()?
                    .flatten();
                let action = if entering {
                    Some(crate::core::WriteBackAction::Close)
                } else if leaving {
                    Some(crate::core::WriteBackAction::Reopen)
                } else {
                    None
                };
                if let Some(action) = action {
                    let subject = linked_subject(txn, &updated.id)?;
                    skipped_writeback(
                        txn,
                        &ids,
                        &clock,
                        &actor.id,
                        action,
                        project.as_ref(),
                        Some(&updated.id),
                        subject.as_deref(),
                        &hop_for_record,
                    )?;
                }
                let mut summary = serde_json::Map::new();
                summary.insert("ref".into(), json!(reference));
                summary.insert("from".into(), json!(from));
                summary.insert("to".into(), json!(to));
                summary.insert("kind".into(), json!(updated.kind.to_string()));
                summary.insert("labels".into(), json!(updated.labels));
                if let Some(project) = project.as_ref() {
                    summary.insert("project".into(), json!(project.slug));
                }
                let payload = serde_json::to_value(&updated).unwrap_or(Value::Null);
                Ok(WriteOutcome::new(
                    updated,
                    "work_item",
                    &fresh.id,
                    payload,
                    WORK_TRANSITIONED,
                    Value::Object(summary),
                ))
            },
        )?;
    }
    Ok(json!({
        "item": current,
        "comments": [],
        "sessions": [],
        "branches": [],
        "git": null,
        "walked": if walk { path } else { Vec::<String>::new() },
        "live_sessions": [],
    }))
}

/// Record a relation between two declared items. `implemented_by` is observed
/// only and refused. A duplicate edge is a conflict. An upstream ref is
/// refused as an invalid request, not a not-found.
fn relate_work<C, I>(ctx: &AppContext<C, I>, params: Value) -> Result<Value, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let reference = text_of(&params, "ref")
        .ok_or_else(|| VogtError::InvalidRequest("ref is required".into()))?;
    let related = text_of(&params, "target")
        .or_else(|| text_of(&params, "related"))
        .ok_or_else(|| VogtError::InvalidRequest("target is required".into()))?;
    let kind_text = text_of(&params, "kind").unwrap_or_else(|| "relates_to".to_string());
    let reason = text_of(&params, "reason").unwrap_or_default();
    if !matches!(
        kind_text.as_str(),
        "depends_on" | "relates_to" | "duplicate_of" | "parent_of"
    ) {
        return Err(VogtError::InvalidRequest(format!(
            "{kind_text} is not a declarable relation; implemented_by is observed from the forge"
        )));
    }
    let kind: crate::core::RelationKind = kind_text.parse().map_err(|_| {
        VogtError::InvalidRequest(format!(
            "unknown relation kind {}",
            crate::core::py_repr(&kind_text)
        ))
    })?;
    let mut writing = crate::application::context::write_of(ctx);
    let clock = std::sync::Arc::clone(writing.clock());
    let stored = audited_write(&mut writing, WORK_RELATE, &reason, move |txn, _actor| {
        let item = declared_only(ctx, txn, &reference)?;
        let other = declared_only(ctx, txn, &related)?;
        if item.id == other.id {
            return Err(VogtError::InvalidRequest(
                "an item cannot relate to itself".into(),
            ));
        }
        if item
            .relations
            .iter()
            .any(|edge| edge.kind == kind && edge.related_id == other.id)
        {
            return Err(VogtError::Conflict(format!(
                "{} already {kind_text} {}",
                item.reference, other.reference
            )));
        }
        if matches!(
            kind,
            crate::core::RelationKind::DependsOn | crate::core::RelationKind::DuplicateOf
        ) && would_cycle(txn, &item.id, &other.id, kind)?
        {
            return Err(VogtError::InvalidRequest(format!(
                "relating {} {kind_text} {} would create a cycle",
                item.reference, other.reference
            )));
        }
        txn.insert_relation(&item.id, &other.id, kind, super::now_of(&clock))?;
        let updated = txn
            .work_item_by_id(&item.id)?
            .ok_or_else(|| VogtError::NotFound(format!("no work item {}", item.reference)))?;
        let edge = json!({"ref": item.reference, "kind": kind_text, "target": other.reference});
        Ok(WriteOutcome::new(
            updated,
            "work_item",
            &item.id,
            edge.clone(),
            WORK_RELATED,
            edge,
        ))
    })?;
    Ok(json!({
        "item": stored, "comments": [], "sessions": [], "branches": [],
        "git": null, "walked": [], "live_sessions": [],
    }))
}

/// Drop one declared relation. An edge that is not there is an invalid
/// request, and an upstream ref is refused rather than reported missing.
fn unrelate_work<C, I>(ctx: &AppContext<C, I>, params: Value) -> Result<Value, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let reference = text_of(&params, "ref")
        .ok_or_else(|| VogtError::InvalidRequest("ref is required".into()))?;
    let related = text_of(&params, "target")
        .or_else(|| text_of(&params, "related"))
        .ok_or_else(|| VogtError::InvalidRequest("target is required".into()))?;
    let kind_text = text_of(&params, "kind").unwrap_or_else(|| "relates_to".to_string());
    let reason = text_of(&params, "reason").unwrap_or_default();
    let kind: crate::core::RelationKind = kind_text.parse().map_err(|_| {
        VogtError::InvalidRequest(format!(
            "unknown relation kind {}",
            crate::core::py_repr(&kind_text)
        ))
    })?;
    let mut writing = crate::application::context::write_of(ctx);
    let stored = audited_write(&mut writing, WORK_UNRELATE, &reason, move |txn, _actor| {
        let item = declared_only(ctx, txn, &reference)?;
        let other = declared_only(ctx, txn, &related)?;
        let removed = txn.delete_relation(&item.id, &other.id, kind)?;
        if !removed {
            return Err(VogtError::InvalidRequest(format!(
                "{} does not {kind_text} {}",
                item.reference, other.reference
            )));
        }
        let updated = txn
            .work_item_by_id(&item.id)?
            .ok_or_else(|| VogtError::NotFound(format!("no work item {}", item.reference)))?;
        let edge = json!({"ref": item.reference, "kind": kind_text, "target": other.reference});
        Ok(WriteOutcome::new(
            updated,
            "work_item",
            &item.id,
            edge.clone(),
            WORK_UNRELATED,
            edge,
        ))
    })?;
    Ok(json!({
        "item": stored, "comments": [], "sessions": [], "branches": [],
        "git": null, "walked": [], "live_sessions": [],
    }))
}

/// True when `target` can already reach `source` through `kind`, so adding the
/// edge would close a cycle.
fn would_cycle(
    txn: &impl crate::storage::interface::ReadView,
    source: &str,
    target: &str,
    kind: crate::core::RelationKind,
) -> Result<bool, VogtError> {
    let mut frontier = vec![target.to_string()];
    let mut seen = std::collections::BTreeSet::from([target.to_string()]);
    while let Some(current) = frontier.pop() {
        if current == source {
            return Ok(true);
        }
        let Some(item) = txn.work_item_by_id(&current)? else {
            continue;
        };
        for relation in item.relations {
            if relation.kind == kind && seen.insert(relation.related_id.clone()) {
                frontier.push(relation.related_id);
            }
        }
    }
    Ok(false)
}

/// Declare the branch a native item is worked on. An omitted name comes from
/// `branch_binding_template`. Declaring the name already recorded is a no-op.
/// An upstream item's overlay write is the write-through and stores nothing.
fn bind_branch<C, I>(ctx: &AppContext<C, I>, params: Value) -> Result<Value, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let reference = text_of(&params, "ref")
        .ok_or_else(|| VogtError::InvalidRequest("ref is required".into()))?;
    let given = text_of(&params, "branch");
    let reason = text_of(&params, "reason").unwrap_or_default();
    // Python resolves the ref inside the write body, so a refused bind opens
    // a transaction and burns its id; the same order keeps the audit ids equal.
    let mut writing = crate::application::context::write_of(ctx);
    let clock = std::sync::Arc::clone(writing.clock());
    let stored = audited_write(&mut writing, WORK_BIND, &reason, move |txn, _actor| {
        // Declared-only `resolve::work_item` misses an upstream subject key.
        // Resolve the same way Python does inside the transaction body.
        let fresh = upstream::resolve_work_ref(ctx, txn, &reference)?;
        let branch = given
            .clone()
            .map(|name| name.trim().to_string())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| {
                crate::branches::default_branch_name(
                    &fresh.reference,
                    &ctx.config.branch_binding_template,
                )
            });
        let Some(project_id) = fresh.project_id.clone() else {
            return Err(VogtError::InvalidRequest(format!(
                "{} belongs to no project, so there is no branch to bind it to",
                fresh.reference
            )));
        };
        let mut overlay = txn.work_overlay(&fresh.reference)?.unwrap_or(WorkOverlay {
            subject_key: fresh.reference.clone(),
            project_id,
            rank: None,
            workflow_state: None,
            priority: None,
            effort: None,
            assignee_actor_id: None,
            initiative_id: None,
            branches: Vec::new(),
            created_at: super::now_of(&clock),
            updated_at: super::now_of(&clock),
        });
        if !overlay.branches.iter().any(|name| name == &branch) {
            overlay.branches.push(branch.clone());
            overlay.updated_at = super::now_of(&clock);
            txn.upsert_work_overlay(&overlay)?;
        }
        let payload = serde_json::to_value(&overlay).unwrap_or(Value::Null);
        let updated = upstream::resolve_work_ref(ctx, txn, &fresh.reference)?;
        Ok(WriteOutcome::new(
            updated,
            "work_item",
            &fresh.id,
            payload,
            WORK_BRANCH_BOUND,
            json!({"ref": fresh.reference, "branch": branch}),
        ))
    })?;
    Ok(json!({
        "item": stored, "comments": [], "sessions": [], "branches": [],
        "git": null, "walked": [], "live_sessions": [],
    }))
}

/// Promote one observed subject into a native work item and record the link.
///
/// The observation must still be the latest for its subject, belong to a
/// project, and not already be linked. The created item takes the observation's
/// kind, title and priority; the link is the audit of the promotion.
fn adopt_work<C, I>(ctx: &AppContext<C, I>, params: Value) -> Result<Value, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let subject = text_of(&params, "subject")
        .ok_or_else(|| VogtError::InvalidRequest("subject is required".into()))?;
    let reason = text_of(&params, "reason").unwrap_or_default();
    // Python's `_subject`: nothing swept is an invalid request, and an unknown
    // subject is a not_found that names it.
    if !ctx.observed.has_evidence_tables()? {
        return Err(VogtError::InvalidRequest(
            "nothing has been swept yet; there are no subjects to adopt".into(),
        ));
    }
    let unknown = || VogtError::NotFound(format!("no observed subject '{subject}'"));
    let observation = ctx
        .observed
        .latest_by_subject(&subject)?
        .ok_or_else(unknown)?;
    let project_id = observation.project_id.clone().ok_or_else(|| {
        VogtError::InvalidRequest(format!(
            "observation {subject} has no project and cannot be adopted"
        ))
    })?;
    let mut writing = crate::application::context::write_of(ctx);
    let clock = std::sync::Arc::clone(writing.clock());
    let ids = std::sync::Arc::clone(writing.ids());
    let (item, link) = audited_write(&mut writing, WORK_ADOPT, &reason, move |txn, _actor| {
        if txn.work_item_by_subject(&subject)?.is_some() {
            return Err(VogtError::InvalidRequest(format!(
                "{subject} is already adopted"
            )));
        }
        let project = txn
            .project_by_id(&project_id)?
            .ok_or_else(|| VogtError::NotFound(format!("no project {project_id}")))?;
        let kind_text = crate::decisions::work_kind_of(&observation);
        let kind: WorkKind = kind_text.parse().unwrap_or(WorkKind::Chore);
        let priority_text = crate::decisions::priority_of(&observation);
        let priority: Priority = priority_text.parse().unwrap_or(Priority::P2);
        let workflow = txn.workflow_for(kind_text)?;
        let now = super::now_of(&clock);
        let id = super::next_id(&ids, "wrk");
        let reference = txn.next_work_ref()?;
        let item = WorkItem {
            id: id.clone(),
            reference: reference.clone(),
            kind,
            title: crate::decisions::title_of(&observation),
            body: String::new(),
            state: workflow.initial_state,
            priority,
            effort: None,
            project_id: Some(project.id.clone()),
            project_slug: Some(project.slug.clone()),
            initiative_id: None,
            origin: crate::core::Origin::Adopted,
            trust_state: crate::core::TrustState::Unverified,
            assignee_actor_id: None,
            assignee_identity_ref: None,
            labels: Vec::new(),
            relations: Vec::new(),
            superseded_by: None,
            created_at: now,
            updated_at: now,
        };
        txn.insert_work_item(&item)?;
        let link = WorkLink {
            work_item_id: id.clone(),
            subject_key: subject.clone(),
            origin_kind: observation.kind.clone(),
            source_url: observation.source_url.clone(),
            relation: crate::core::LinkRelation::Completion,
            created_at: now,
        };
        txn.insert_work_link(&link)?;
        let payload = serde_json::to_value(&item).unwrap_or(Value::Null);
        Ok(WriteOutcome::new(
            (item, link),
            "work_item",
            &id,
            payload,
            WORK_ADOPTED,
            json!({"ref": reference, "subject_key": subject}),
        ))
    })?;
    Ok(json!({"item": item, "link": link}))
}

#[allow(clippy::too_many_arguments)]
fn skipped_writeback(
    txn: &mut dyn WriteTxn,
    ids: &std::sync::Arc<std::sync::Mutex<impl IdFactory>>,
    clock: &std::sync::Arc<std::sync::Mutex<impl Clock>>,
    actor_id: &str,
    action: crate::core::WriteBackAction,
    project: Option<&Project>,
    work_item_id: Option<&str>,
    subject_key: Option<&str>,
    reason: &str,
) -> Result<WriteBackRecord, VogtError> {
    let policy = project
        .map(|project| project.write_back.to_string())
        .unwrap_or_else(|| "none".into());
    let permitted = matches!(
        (policy.as_str(), action),
        ("full", _) | ("comment_only", crate::core::WriteBackAction::Comment)
    );
    let detail = if !permitted {
        if policy == "none" {
            "write-back is off for this project".to_string()
        } else {
            format!("policy is {policy:?}; {} is not permitted", action)
        }
    } else if subject_key.is_none() && action != crate::core::WriteBackAction::Create {
        "this item is not linked to a forge object".to_string()
    } else {
        "no forge is configured to write to for this project".to_string()
    };
    let record = WriteBackRecord {
        id: super::next_id(ids, "wbk"),
        at: super::now_of(clock),
        project_id: project.map(|project| project.id.clone()),
        work_item_id: work_item_id.map(str::to_string),
        actor_id: actor_id.to_string(),
        action,
        subject_key: subject_key.map(str::to_string),
        policy,
        outcome: crate::core::WriteBackOutcome::Skipped,
        reason: reason.to_string(),
        detail: Some(detail),
        source_url: None,
    };
    txn.insert_writeback(&record)?;
    Ok(record)
}

fn linked_subject(txn: &impl ReadView, item_id: &str) -> Result<Option<String>, VogtError> {
    Ok(txn
        .work_links_for_subjects_by_item(item_id)?
        .into_keys()
        .next())
}

/// A ref that must name a declared row. An upstream-truth item is refused
/// with the reason named, rather than a bare not-found.
fn declared_only<C: Clock, I: IdFactory>(
    ctx: &AppContext<C, I>,
    view: &impl ReadView,
    reference: &str,
) -> Result<WorkItem, VogtError> {
    match resolve::work_item(view, reference) {
        Ok(item) => Ok(item),
        Err(VogtError::NotFound(_)) => {
            if upstream::resolve_upstream(ctx, view, reference)?.is_some() {
                Err(VogtError::InvalidRequest(format!(
                    "{reference} is an upstream-truth item; relations between upstream issues are not supported in v1 of the upstream-truth model — relate them on the forge"
                )))
            } else {
                Err(VogtError::NotFound(format!(
                    "no work item {}",
                    crate::core::py_repr(reference)
                )))
            }
        }
        Err(other) => Err(other),
    }
}

fn board_fingerprint(params: &Value) -> String {
    let mut body = serde_json::Map::new();
    for key in ["project", "assignee", "initiative", "label"] {
        body.insert(key.into(), params.get(key).cloned().unwrap_or(Value::Null));
    }
    for key in ["kinds", "states", "priorities"] {
        let mut values = strings(params.get(key));
        values.sort();
        body.insert(key.into(), json!(values));
    }
    body.insert(
        "lane_mode".into(),
        json!(params
            .get("lane_mode")
            .and_then(Value::as_str)
            .unwrap_or("none")),
    );
    crate::decisions::digest_of(&Value::Object(body))
}

fn board_token(value: &Value) -> String {
    use base64::Engine;
    // `json.dumps(..., sort_keys=True, separators=(",", ":"))`, then urlsafe
    // base64 with the padding stripped. Not the spaced form the stores use.
    let raw = crate::decisions::render_python_json_compact(value);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw.as_bytes())
}

fn text_of(params: &Value, key: &str) -> Option<String> {
    params.get(key).and_then(Value::as_str).map(str::to_string)
}

/// A bounded, snapshot-stable Board for native items.
///
/// An unlinked project answers with empty cells and `link_state: "unlinked"`.
/// The upstream half of a linked board is gathered the same way `work.list`
/// gathers it and merged into the counts; the snapshot token carries the
/// declared revision so a continuation after a write is refused.
fn list_board<C: Clock, I: IdFactory>(
    ctx: &AppContext<C, I>,
    params: Value,
) -> Result<Value, VogtError> {
    let cells = params
        .get("cells")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if cells.is_empty() {
        return Err(VogtError::InvalidRequest(
            "board.list needs at least one cell".to_string(),
        ));
    }
    let lane_mode = params
        .get("lane_mode")
        .and_then(Value::as_str)
        .unwrap_or("none");
    let page_size = params
        .get("page_size")
        .and_then(Value::as_i64)
        .unwrap_or(25);
    let view = ctx.declared.read()?;
    let project = params
        .get("project")
        .and_then(Value::as_str)
        .map(|slug| resolve::project(&view, slug))
        .transpose()?;
    if let Some(project) = project.as_ref() {
        if !upstream::is_linked(project) {
            let pending = view.count_work_items(&WorkFilter {
                project_id: Some(project.id.clone()),
                exclude_terminal: true,
                ..WorkFilter::default()
            })?;
            let empty: Vec<Value> = cells
                .iter()
                .map(|cell| {
                    json!({
                        "lane_key": cell.get("lane_key").and_then(Value::as_str).unwrap_or(""),
                        "state": cell.get("state").and_then(Value::as_str).unwrap_or(""),
                        "items": [],
                        "total": 0,
                        "next_cursor": null,
                    })
                })
                .collect();
            let snapshot_at = super::now_of(&ctx.clock);
            let revision = view.current_revision()?;
            let snapshot = board_token(&json!({
                "kind": "snapshot",
                "fingerprint": board_fingerprint(&params),
                "revision": revision,
                "snapshot_at": snapshot_at.to_iso(),
                "high_water": Value::Null,
            }));
            return Ok(json!({
                "cells": empty,
                "column_totals": {},
                "lane_totals": {},
                "total": 0,
                "backlog_candidates": 0,
                "declared_total": 0,
                "link_state": "unlinked",
                "excluded_unlinked": pending,
                "snapshot": snapshot,
                "snapshot_at": snapshot_at,
                "revision": revision,
            }));
        }
    }
    let filter = WorkFilter {
        project_id: project.as_ref().map(|project| project.id.clone()),
        kinds: strings(params.get("kinds")),
        states: strings(params.get("states")),
        priorities: strings(params.get("priorities")),
        ..WorkFilter::default()
    };
    let revision = view.current_revision()?;
    let high_water = view.board_high_water(&filter)?;
    let queries: Vec<crate::storage::interface::BoardCellQuery> = cells
        .iter()
        .map(|cell| crate::storage::interface::BoardCellQuery {
            lane_key: cell
                .get("lane_key")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            state: cell
                .get("state")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            after_created_at: None,
            after_ref: None,
        })
        .collect();
    let counts = view.board_counts(&filter, lane_mode, high_water.as_ref())?;
    let pages = view.board_work_items(
        &filter,
        lane_mode,
        &queries,
        high_water.as_ref(),
        page_size + 1,
    )?;
    let mut column_totals = serde_json::Map::new();
    let mut lane_totals = serde_json::Map::new();
    let mut total = 0_i64;
    for ((lane, state), count) in &counts {
        total += count;
        let column = column_totals.entry(state.clone()).or_insert(json!(0));
        *column = json!(column.as_i64().unwrap_or(0) + count);
        let lane_total = lane_totals.entry(lane.clone()).or_insert(json!(0));
        *lane_total = json!(lane_total.as_i64().unwrap_or(0) + count);
    }
    let rendered: Vec<Value> = queries
        .iter()
        .map(|query| {
            let key = (query.lane_key.clone(), query.state.clone());
            let fetched = pages.get(&key).cloned().unwrap_or_default();
            let shown: Vec<&WorkItem> = fetched.iter().take(page_size.max(0) as usize).collect();
            json!({
                "lane_key": query.lane_key,
                "state": query.state,
                "items": shown,
                "total": counts.get(&key).copied().unwrap_or(0),
                "next_cursor": if fetched.len() > shown.len() { Value::String("more".into()) } else { Value::Null },
            })
        })
        .collect();
    let snapshot_at = super::now_of(&ctx.clock);
    let snapshot = board_token(&json!({
        "kind": "snapshot",
        "fingerprint": board_fingerprint(&params),
        "revision": revision,
        "snapshot_at": snapshot_at.to_iso(),
        "high_water": high_water.as_ref().map(|(moment, reference)| json!([moment.to_iso(), reference])),
    }));
    // Python's `candidate_population`, read after the snapshot as Python reads
    // the clock: the observed-inclusive count and the declared-only count.
    let (backlog_candidates, declared_total) = crate::application::views::candidate_population(
        ctx,
        params.get("project").and_then(Value::as_str),
        strings(params.get("kinds")),
        strings(params.get("priorities")),
        params.get("assignee").and_then(Value::as_str),
        params.get("initiative").and_then(Value::as_str),
        params.get("label").and_then(Value::as_str),
    )?;
    Ok(json!({
        "cells": rendered,
        "column_totals": column_totals,
        "lane_totals": lane_totals,
        "total": total,
        "backlog_candidates": backlog_candidates,
        "declared_total": declared_total,
        "link_state": project.as_ref().map(|_| "linked"),
        "excluded_unlinked": 0,
        "snapshot": snapshot,
        "snapshot_at": snapshot_at,
        "revision": revision,
    }))
}

#[cfg(test)]
mod tests {
    use super::{
        bind_branch, comment_work, create_work, get_many_work, get_work, list_board, list_work,
        relate_work, transition_work, unrelate_work, update_work, work_page,
    };
    use crate::application::context::{build_context, Built};
    use crate::core::{
        ActorKind, LinkState, Moment, Principal, Project, SequentialIds, StepClock, WriteBack,
    };
    use crate::errors::VogtError;
    use crate::storage::interface::{DeclaredStore, ObservedStore, ReadView, WriteTxn};
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

    #[test]
    fn a_comment_is_returned_by_the_get() {
        let built = opened();
        let Built::StepSequential(ctx) = &built else {
            unreachable!("opened() builds a step clock");
        };
        create_work(
            ctx,
            json!({"kind": "chore", "title": "one", "reason": "test"}),
        )
        .unwrap();
        let commented = comment_work(
            ctx,
            json!({"ref": "WI-1", "body": "noted", "reason": "test"}),
        )
        .unwrap();
        assert_eq!(commented["comment"]["body"], "noted");
        assert_eq!(commented["write_back"], "skipped");
        assert_eq!(commented["comment"]["kind"], "note");
        let fetched = get_work(ctx, json!({"ref": "WI-1"})).unwrap();
        assert_eq!(fetched["comments"][0]["body"], "noted");
        assert_eq!(fetched["item"]["ref"], "WI-1");
    }

    #[test]
    fn a_walk_records_each_hop_and_a_cycle_is_refused() {
        let built = opened();
        let Built::StepSequential(ctx) = &built else {
            unreachable!("opened() builds a step clock");
        };
        create_work(
            ctx,
            json!({"kind": "chore", "title": "one", "reason": "test"}),
        )
        .unwrap();
        create_work(
            ctx,
            json!({"kind": "chore", "title": "two", "reason": "test"}),
        )
        .unwrap();
        let walked = transition_work(
            ctx,
            json!({"ref": "WI-1", "to_state": "review", "walk": true, "reason": "test"}),
        )
        .unwrap();
        assert_eq!(walked["item"]["state"], "review");
        assert_eq!(walked["walked"].as_array().unwrap().len(), 3);
        relate_work(
            ctx,
            json!({"ref": "WI-1", "related": "WI-2", "kind": "depends_on", "reason": "test"}),
        )
        .unwrap();
        let cycle = relate_work(
            ctx,
            json!({"ref": "WI-2", "related": "WI-1", "kind": "depends_on", "reason": "test"}),
        )
        .unwrap_err();
        assert!(matches!(cycle, VogtError::InvalidRequest(_)), "{cycle}");
        unrelate_work(
            ctx,
            json!({"ref": "WI-1", "related": "WI-2", "kind": "depends_on", "reason": "test"}),
        )
        .unwrap();
    }

    #[test]
    fn binding_a_branch_is_idempotent_and_the_board_counts_it() {
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
        let bound = bind_branch(
            ctx,
            json!({"ref": "WI-1", "branch": "feat", "reason": "test"}),
        )
        .unwrap();
        assert_eq!(bound["item"]["ref"], "WI-1");
        bind_branch(
            ctx,
            json!({"ref": "WI-1", "branch": "feat", "reason": "test"}),
        )
        .unwrap();
        let board = list_board(ctx, json!({"cells": [{"lane_key": "", "state": "open"}]})).unwrap();
        assert_eq!(board["total"], 1);
        assert_eq!(board["cells"][0]["items"][0]["ref"], "WI-1");
    }

    #[test]
    fn binding_a_branch_to_an_upstream_subject_stores_the_overlay() {
        let built = opened();
        let Built::StepSequential(ctx) = &built else {
            unreachable!("opened() builds a step clock");
        };
        insert_project(&built, "beta", true, WriteBack::Disabled);
        let sweep = ctx
            .observed
            .begin_sweep("forge", &["prj_beta".into()], moment())
            .unwrap();
        ctx.observed
            .append(
                &sweep.id,
                &[crate::storage::observed_types::PendingObservation {
                    kind: "forge.issue".into(),
                    subject_key: "gh:acme/beta#7".into(),
                    payload: serde_json::json!({
                        "title": "upstream",
                        "state": "open",
                        "number": 7
                    }),
                    content_digest: "sha256:issue-7".into(),
                    project_id: Some("prj_beta".into()),
                    source_url: Some("https://github.com/acme/beta/issues/7".into()),
                    promoted: false,
                }],
                moment(),
            )
            .unwrap();
        ctx.observed.rebuild_latest().unwrap();
        let bound = bind_branch(
            ctx,
            json!({"ref": "gh:acme/beta#7", "branch": "feat/upstream", "reason": "test"}),
        )
        .unwrap();
        assert_eq!(bound["item"]["ref"], "gh:acme/beta#7");
        assert_eq!(bound["item"]["project_id"], "prj_beta");
        let view = ctx.declared.read().unwrap();
        let overlay = view.work_overlay("gh:acme/beta#7").unwrap().unwrap();
        assert_eq!(overlay.branches, vec!["feat/upstream".to_string()]);
        drop(view);
        bind_branch(
            ctx,
            json!({"ref": "gh:acme/beta#7", "branch": "feat/upstream", "reason": "test"}),
        )
        .unwrap();
        let view = ctx.declared.read().unwrap();
        let again = view.work_overlay("gh:acme/beta#7").unwrap().unwrap();
        assert_eq!(again.branches, vec!["feat/upstream".to_string()]);
    }

    #[test]
    fn get_many_reads_rows_in_the_order_asked_and_names_the_missing() {
        let built = opened();
        let Built::StepSequential(ctx) = &built else {
            unreachable!("opened() builds a step clock");
        };
        create_work(
            ctx,
            json!({"kind": "feature", "title": "First", "reason": "test"}),
        )
        .unwrap();
        create_work(
            ctx,
            json!({"kind": "feature", "title": "Second", "reason": "test"}),
        )
        .unwrap();
        transition_work(
            ctx,
            json!({"ref": "WI-2", "to_state": "in_progress", "reason": "test"}),
        )
        .unwrap();

        let answer =
            get_many_work(ctx, json!({"refs": ["WI-2", "WI-404", "WI-1", "WI-2"]})).unwrap();

        let refs: Vec<&str> = answer["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["ref"].as_str().unwrap())
            .collect();
        assert_eq!(refs, ["WI-2", "WI-1"]);
        assert_eq!(answer["items"][0]["title"], "Second");
        assert_eq!(answer["items"][0]["state"], "in_progress");
        assert_eq!(answer["items"][0]["kind"], "feature");
        assert_eq!(answer["missing"], json!(["WI-404"]));
    }

    #[test]
    fn get_many_reads_a_finished_item_like_any_other() {
        let built = opened();
        let Built::StepSequential(ctx) = &built else {
            unreachable!("opened() builds a step clock");
        };
        create_work(
            ctx,
            json!({"kind": "feature", "title": "Shipped", "reason": "test"}),
        )
        .unwrap();
        transition_work(
            ctx,
            json!({"ref": "WI-1", "to_state": "wont_do", "reason": "test"}),
        )
        .unwrap();

        let answer = get_many_work(ctx, json!({"refs": ["WI-1"]})).unwrap();

        assert_eq!(answer["items"][0]["ref"], "WI-1");
        assert_eq!(answer["items"][0]["state"], "wont_do");
    }

    #[test]
    fn get_many_bounds_the_batch() {
        let built = opened();
        let Built::StepSequential(ctx) = &built else {
            unreachable!("opened() builds a step clock");
        };
        assert!(get_many_work(ctx, json!({"refs": []})).is_err());
        assert!(get_many_work(ctx, json!({"refs": [""]})).is_err());
        let too_many: Vec<String> = (0..=super::WORK_GET_MANY_LIMIT)
            .map(|n| format!("WI-{n}"))
            .collect();
        assert!(get_many_work(ctx, json!({"refs": too_many})).is_err());
        let at_the_limit: Vec<String> = (0..super::WORK_GET_MANY_LIMIT)
            .map(|n| format!("WI-{n}"))
            .collect();
        let answer = get_many_work(ctx, json!({"refs": at_the_limit})).unwrap();
        assert_eq!(
            answer["missing"].as_array().unwrap().len(),
            super::WORK_GET_MANY_LIMIT
        );
    }

    #[test]
    fn an_exact_ref_query_leads_the_page_ahead_of_substring_hits() {
        let built = opened();
        let Built::StepSequential(ctx) = &built else {
            unreachable!("opened() builds a step clock");
        };
        // WI-1 is created first and cites WI-10 in its body, so a substring
        // match on "10" reaches it before WI-10 in creation order.
        create_work(
            ctx,
            json!({"kind": "feature", "title": "An older note", "body": "see WI-10 for the real one", "reason": "test"}),
        )
        .unwrap();
        for n in 2..=10 {
            create_work(
                ctx,
                json!({"kind": "chore", "title": format!("Item {n}"), "reason": "test"}),
            )
            .unwrap();
        }

        for query in ["10", "WI-10", "wi-10"] {
            let answer = list_work(ctx, json!({"query": query, "limit": 1})).unwrap();
            assert_eq!(answer["items"][0]["ref"], "WI-10", "query {query}");
            assert_eq!(answer["total"], 2, "query {query}");
        }
        let whole = list_work(ctx, json!({"query": "10"})).unwrap();
        let refs: Vec<&str> = whole["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["ref"].as_str().unwrap())
            .collect();
        assert_eq!(refs, ["WI-10", "WI-1"]);
    }
}
