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

use crate::application::context::AppContext;
use crate::application::services::{context_parts, next_id, now_of, write_context};
use crate::application::writes::{audited_write, WriteOutcome};
use crate::core::{slugify, Clock, IdFactory, Initiative, InitiativeState};
use crate::errors::VogtError;
use crate::storage::interface::{DeclaredStore, ReadView, WriteTxn};
use std::sync::Arc;

crate::typed_op!(initiative_create_op, CreateParams, initiative_create);
crate::typed_op!(initiative_list_op, ListParams, initiative_list);
crate::typed_op!(initiative_update_op, UpdateParams, initiative_update);
crate::typed_op!(initiative_publish_op, PublishParams, initiative_publish);

/// Raw wire fields. Each is held as a JSON value so the typed parameter types
/// below can coerce them with the exact messages the hand-rolled readers gave.
/// `null` reads as absent, as it always has. A body that is not an object reads
/// as no fields at all.
#[derive(serde::Deserialize, Default)]
struct Raw {
    #[serde(default)]
    slug: Option<Value>,
    #[serde(default)]
    title: Option<Value>,
    #[serde(default)]
    body: Option<Value>,
    #[serde(default)]
    state: Option<Value>,
    #[serde(default)]
    weight: Option<Value>,
    #[serde(default)]
    reason: Option<Value>,
    #[serde(default)]
    limit: Option<Value>,
    #[serde(default)]
    offset: Option<Value>,
    #[serde(default)]
    mode: Option<Value>,
}

fn raw_of<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Raw, D::Error> {
    use serde::de::Error;
    match <Value as serde::Deserialize>::deserialize(deserializer)? {
        Value::Object(map) => serde_json::from_value(Value::Object(map)).map_err(D::Error::custom),
        _ => Ok(Raw::default()),
    }
}

fn required(field: &str, value: &Option<Value>) -> Result<String, VogtError> {
    match value.as_ref().and_then(Value::as_str) {
        Some(text) if !text.trim().is_empty() => Ok(text.to_string()),
        _ => Err(VogtError::InvalidRequest(format!("{field} is required"))),
    }
}

fn string_or_none(field: &str, value: &Option<Value>) -> Result<Option<String>, VogtError> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => Ok(Some(text.clone())),
        Some(_) => Err(VogtError::InvalidRequest(format!(
            "{field} must be a string"
        ))),
    }
}

fn integer_or_none(field: &str, value: &Option<Value>) -> Result<Option<i64>, VogtError> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(number)) => number
            .as_i64()
            .map(Some)
            .ok_or_else(|| VogtError::InvalidRequest(format!("{field} must be an integer"))),
        Some(_) => Err(VogtError::InvalidRequest(format!(
            "{field} must be an integer"
        ))),
    }
}

fn custom<E: serde::de::Error>(err: VogtError) -> E {
    match err {
        VogtError::InvalidRequest(message) => E::custom(message),
        other => E::custom(other),
    }
}

/// `initiative.create` parameters. Absent and `null` both mean "use the default".
struct CreateParams {
    title: String,
    body: String,
    state: String,
    weight: i64,
    reason: String,
}

impl CreateParams {
    fn from_raw(raw: &Raw) -> Result<Self, VogtError> {
        let title = required("title", &raw.title)?;
        let body = string_or_none("body", &raw.body)?.unwrap_or_default();
        let state = string_or_none("state", &raw.state)?.unwrap_or_else(|| "open".to_string());
        let weight = integer_or_none("weight", &raw.weight)?.unwrap_or(0);
        let reason = required("reason", &raw.reason)?;
        Ok(Self {
            title,
            body,
            state,
            weight,
            reason,
        })
    }
}

impl<'de> serde::Deserialize<'de> for CreateParams {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::from_raw(&raw_of(deserializer)?).map_err(custom)
    }
}

/// `initiative.list` parameters.
struct ListParams {
    limit: i64,
    offset: i64,
    summary: bool,
}

impl<'de> serde::Deserialize<'de> for ListParams {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = raw_of(deserializer)?;
        let read = || -> Result<Self, VogtError> {
            Ok(Self {
                limit: integer_or_none("limit", &raw.limit)?.unwrap_or(100),
                offset: integer_or_none("offset", &raw.offset)?.unwrap_or(0),
                // Only the exact string `summary` blanks bodies.
                summary: raw.mode.as_ref().and_then(Value::as_str) == Some("summary"),
            })
        };
        read().map_err(custom)
    }
}

/// `initiative.update` parameters. A field that is absent or `null` is not
/// changed, so `None` covers both; the guard against an empty update runs here.
struct UpdateParams {
    slug: String,
    title: Option<String>,
    body: Option<String>,
    weight: Option<i64>,
    state: Option<InitiativeState>,
    reason: String,
}

impl<'de> serde::Deserialize<'de> for UpdateParams {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = raw_of(deserializer)?;
        let read = || -> Result<Self, VogtError> {
            let slug = required("slug", &raw.slug)?;
            let title = string_or_none("title", &raw.title)?;
            let body = string_or_none("body", &raw.body)?;
            let weight = integer_or_none("weight", &raw.weight)?;
            let state = match string_or_none("state", &raw.state)? {
                Some(text) => Some(parse_state(&text)?),
                None => None,
            };
            let reason = required("reason", &raw.reason)?;
            if title.is_none() && body.is_none() && weight.is_none() && state.is_none() {
                return Err(VogtError::InvalidRequest(
                    "give a title, body, weight or state; there is nothing else to update"
                        .to_string(),
                ));
            }
            Ok(Self {
                slug,
                title,
                body,
                weight,
                state,
                reason,
            })
        };
        read().map_err(custom)
    }
}

/// `initiative.publish` parameters.
struct PublishParams {
    slug: String,
    reason: String,
}

impl<'de> serde::Deserialize<'de> for PublishParams {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = raw_of(deserializer)?;
        let read = || -> Result<Self, VogtError> {
            Ok(Self {
                slug: required("slug", &raw.slug)?,
                reason: required("reason", &raw.reason)?,
            })
        };
        read().map_err(custom)
    }
}

fn initiative_create<C, I>(ctx: &AppContext<C, I>, params: CreateParams) -> Result<Value, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let CreateParams {
        title,
        body,
        state,
        weight,
        reason,
    } = params;
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

fn initiative_list<C, I>(ctx: &AppContext<C, I>, params: ListParams) -> Result<Value, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let (declared, _, _, _, _, _) = context_parts(ctx);
    let view = declared.read()?;
    let rows = view.list_initiatives(params.limit, params.offset)?;
    // `summary` blanks each body. Anything else, including the default `full`,
    // keeps it. Python copies the row and sets body to "" only in that case.
    let summary = params.summary;
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

fn initiative_update<C, I>(ctx: &AppContext<C, I>, params: UpdateParams) -> Result<Value, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let UpdateParams {
        slug,
        title,
        body,
        weight,
        state,
        reason,
    } = params;
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

/// `initiative.publish`. Ports `publish_initiative`: one tracking issue per
/// forge-linked repo the initiative spans. A repo that cannot be written is a
/// `skipped` row, not a silent drop. Closing the initiative raises a drift
/// proposal; it never closes the upstream issue.
fn initiative_publish<C, I>(
    ctx: &AppContext<C, I>,
    params: PublishParams,
) -> Result<Value, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let PublishParams { slug, reason } = params;
    let (initiative, targets, mut skipped) = {
        let view = ctx.declared.read()?;
        let initiative = view.initiative_by_slug(&slug)?.ok_or_else(|| {
            VogtError::NotFound(format!(
                "no initiative with slug {}",
                crate::core::py_repr(&slug)
            ))
        })?;
        let actor = view.actor_by_identity(&ctx.principal.identity_ref)?;
        let (targets, skipped) = publish_targets(ctx, &view, actor.as_ref(), &initiative)?;
        (initiative, targets, skipped)
    };
    let mut placed = Vec::new();
    for target in targets {
        placed.push(find_or_create(target, &initiative)?);
    }
    let mut rows = reproject_placed(ctx, &initiative, &placed, true)?;
    rows.append(&mut skipped);
    let result = json!({
        "slug": initiative.slug,
        "state": initiative.state.to_string(),
        "tracking_issues": rows,
    });
    let mut writing = crate::application::context::write_of(ctx);
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

    let rows = reproject_placed(ctx, &initiative, &placed, false)?;
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
    /// `adopted` when the marker already existed, `created` when this call opened it.
    action: &'static str,
}

/// Far above any plausible initiative membership. Ports `_MEMBER_LIMIT`.
const MEMBER_LIMIT: i64 = 10_000;

/// The linked projects holding a member that can be written. Ports `_targets`
/// without the skipped rows, which only `initiative.publish` reports.
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
    Ok(publish_targets(ctx, view, actor, initiative)?.0)
}

/// Ports `_targets`, keeping the `skipped` rows `initiative.publish` returns.
fn publish_targets<C, I>(
    ctx: &AppContext<C, I>,
    view: &impl ReadView,
    actor: Option<&crate::core::Actor>,
    initiative: &Initiative,
) -> Result<(Vec<ProjectionTarget>, Vec<Value>), VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    use crate::adapters::forge::ForgeProvider;
    let mut targets = Vec::new();
    let mut skipped = Vec::new();
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
            skipped.push(skip_row(
                &project,
                &format!(
                    "write-back policy {} does not permit opening a tracking issue — set it with `forge writeback`",
                    crate::core::py_repr(&project.write_back.to_string())
                ),
            ));
            continue;
        }
        let (provider, identity) = crate::application::services::forge_writeback::writer_provider(
            ctx,
            actor,
            project.repo_url.as_deref(),
        )?;
        let Some(provider) = provider else {
            skipped.push(skip_row(
                &project,
                "no usable forge credential for its repository",
            ));
            continue;
        };
        let Some(repo) = provider.parse(project.repo_url.as_deref()) else {
            skipped.push(skip_row(
                &project,
                "no usable forge credential for its repository",
            ));
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
    Ok((targets, skipped))
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
/// Ports `_reproject_placed`. `propose_close` raises a drift proposal per
/// tracking issue when the initiative is closed; it never closes upstream.
fn reproject_placed<C, I>(
    ctx: &AppContext<C, I>,
    initiative: &Initiative,
    placed: &[PlacedIssue],
    propose_close: bool,
) -> Result<Vec<Value>, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
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
    let mut close_targets: Vec<&PlacedIssue> = Vec::new();
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
        let close_here = propose_close && initiative.state == crate::core::InitiativeState::Closed;
        rows.push(json!({
            "project_slug": item.target.project.slug,
            "repo_url": item.target.project.repo_url,
            "number": item.number,
            "source_url": item.source_url,
            "action": item.action,
            "members": item.target.members.len() as i64,
            "detail": detail,
            "close_proposed": close_here,
        }));
        if close_here {
            close_targets.push(item);
        }
    }
    if !close_targets.is_empty() {
        propose_closes(ctx, initiative, &close_targets)?;
    }
    Ok(rows)
}

const INITIATIVE_TRACKING_CLOSE: &str = "initiative_tracking_close";

fn skip_row(project: &crate::core::Project, detail: &str) -> Value {
    json!({
        "project_slug": project.slug,
        "repo_url": project.repo_url,
        "number": null,
        "source_url": null,
        "action": "skipped",
        "members": 0,
        "detail": detail,
        "close_proposed": false,
    })
}

fn projection_failed(slug: &str, detail: Option<&str>) -> VogtError {
    VogtError::UpstreamWriteFailed(format!(
        "projecting the initiative onto {} failed: {} — nothing was closed or deleted, and re-running `initiative publish` resumes",
        crate::core::py_repr(slug),
        detail.unwrap_or("no detail from the provider"),
    ))
}

/// Adopt the marked tracking issue, or open one if there is none yet.
fn find_or_create(
    target: ProjectionTarget,
    initiative: &Initiative,
) -> Result<PlacedIssue, VogtError> {
    use crate::adapters::forge::ForgeProvider;
    if let Some(existing) = find_existing_ref(&target, initiative)? {
        return Ok(existing_into(target, existing));
    }
    let region = crate::decisions::render_managed_region(
        &initiative.slug,
        &initiative.body,
        &task_lines(&target),
        &[],
    );
    let label = format!("initiative:{}", initiative.slug);
    let sent =
        target
            .provider
            .create_issue(&target.repo, &initiative.title, &region, Some(&[label]))?;
    let number = sent
        .subject_key
        .as_deref()
        .and_then(|key| target.provider.number_of(Some(key)));
    if sent.outcome != crate::adapters::forge::WriteBackOutcome::Succeeded || number.is_none() {
        return Err(projection_failed(
            &target.project.slug,
            sent.detail.as_deref(),
        ));
    }
    Ok(PlacedIssue {
        number: number.expect("checked"),
        source_url: sent.source_url,
        current_body: region,
        target,
        action: "created",
    })
}

struct FoundIssue {
    number: i64,
    source_url: Option<String>,
    current_body: String,
}

fn find_existing_ref(
    target: &ProjectionTarget,
    initiative: &Initiative,
) -> Result<Option<FoundIssue>, VogtError> {
    use crate::adapters::forge::ForgeProvider;
    use crate::adapters::forge::IssuesItem;
    let issues = target.provider.issues_updated_since(&target.repo, None)?;
    for issue in issues.into_iter().filter_map(IssuesItem::into_issue) {
        if crate::decisions::body_has_marker(issue.body.as_deref(), &initiative.slug) {
            return Ok(Some(FoundIssue {
                number: issue.number,
                source_url: issue.source_url,
                current_body: issue.body.unwrap_or_default(),
            }));
        }
    }
    Ok(None)
}

fn existing_into(target: ProjectionTarget, found: FoundIssue) -> PlacedIssue {
    PlacedIssue {
        target,
        number: found.number,
        source_url: found.source_url,
        current_body: found.current_body,
        action: "adopted",
    }
}

fn find_existing(
    target: ProjectionTarget,
    initiative: &Initiative,
) -> Result<Option<PlacedIssue>, VogtError> {
    Ok(find_existing_ref(&target, initiative)?.map(|found| existing_into(target, found)))
}

fn propose_closes<C, I>(
    ctx: &AppContext<C, I>,
    initiative: &Initiative,
    placed: &[&PlacedIssue],
) -> Result<(), VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    use crate::adapters::forge::ForgeProvider;
    let open_now = ctx.declared.read()?.list_drift(
        Some("open"),
        Some(INITIATIVE_TRACKING_CLOSE),
        None,
        MEMBER_LIMIT,
    )?;
    let already: std::collections::BTreeSet<String> = open_now
        .iter()
        .filter(|proposal| proposal.subject_id == initiative.id)
        .filter_map(|proposal| {
            proposal
                .proposed_change
                .get("subject_key")
                .and_then(|value| value.as_str())
                .map(str::to_string)
        })
        .collect();
    for item in placed {
        let subject_key = item
            .target
            .provider
            .subject_key(&item.target.repo, item.number);
        if already.contains(&subject_key) {
            continue;
        }
        raise_close(ctx, initiative, item, &subject_key)?;
    }
    Ok(())
}

fn raise_close<C, I>(
    ctx: &AppContext<C, I>,
    initiative: &Initiative,
    item: &PlacedIssue,
    subject_key: &str,
) -> Result<(), VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let id = ctx
        .id_factory
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .next("dft");
    let opened_at = ctx
        .clock
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .now();
    let proposal = crate::core::DriftProposal {
        id,
        kind: INITIATIVE_TRACKING_CLOSE.to_string(),
        subject_kind: "initiative".to_string(),
        subject_id: initiative.id.clone(),
        project_id: Some(item.target.project.id.clone()),
        project_slug: Some(item.target.project.slug.clone()),
        summary: format!(
            "initiative {} is closed; its tracking issue #{} in {} is still open upstream — Vogt proposes closing it, never writes the close",
            crate::core::py_repr(&initiative.slug),
            item.number,
            crate::core::py_repr(&item.target.project.slug),
        ),
        evidence_observation_id: None,
        evidence_snapshot: Value::Null,
        proposed_change: json!({
            "entity": "tracking_issue",
            "action": "close",
            "initiative": initiative.slug,
            "subject_key": subject_key,
            "number": item.number,
            "source_url": item.source_url,
        }),
        status: crate::core::DriftStatus::Open,
        opened_at,
        superseded_at: None,
        superseded_detail: None,
        resolved_by_actor_id: None,
        resolved_by_identity_ref: None,
        resolved_at: None,
        resolution_reason: None,
    };
    let reason = format!(
        "initiative {} closed: propose tracking-issue close",
        crate::core::py_repr(&initiative.slug)
    );
    let mut write = crate::application::context::write_of(ctx);
    crate::application::writes::audited_write(
        &mut write,
        "initiative.publish",
        &reason,
        move |txn, _actor| {
            txn.insert_drift(&proposal)?;
            Ok(crate::application::writes::WriteOutcome {
                result: serde_json::to_value(&proposal).unwrap_or(Value::Null),
                entity_kind: "drift_proposal".to_string(),
                entity_id: proposal.id.clone(),
                payload: serde_json::to_value(&proposal).unwrap_or(Value::Null),
                event_kind: "drift.raised".to_string(),
                summary: json!({"kind": proposal.kind, "initiative": initiative.slug}),
            })
        },
    )?;
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::{
        initiative_create_op, initiative_list_op, initiative_publish_op, initiative_update_op,
    };
    use crate::application::services::services_test::opened;
    use crate::errors::VogtError;
    use serde_json::json;

    fn refused(result: Result<serde_json::Value, VogtError>) -> String {
        match result {
            Err(VogtError::InvalidRequest(message)) => message,
            other => panic!("expected InvalidRequest, got {other:?}"),
        }
    }

    /// Every coercion message is pinned byte for byte, as the hand-rolled
    /// readers wrote them, including the order when two fields are wrong.
    #[test]
    fn decode_messages_are_unchanged() {
        let built = opened();
        let create = |v| refused(initiative_create_op(&built, v));
        assert_eq!(create(json!({"reason": "r"})), "title is required");
        assert_eq!(
            create(json!({"title": "  ", "reason": "r"})),
            "title is required"
        );
        assert_eq!(
            create(json!({"title": 3, "reason": "r"})),
            "title is required"
        );
        assert_eq!(create(json!(null)), "title is required");
        assert_eq!(create(json!({"title": "T"})), "reason is required");
        assert_eq!(
            create(json!({"title": "T", "reason": "r", "body": 1})),
            "body must be a string"
        );
        assert_eq!(
            create(json!({"title": "T", "reason": "r", "state": true})),
            "state must be a string"
        );
        assert_eq!(
            create(json!({"title": "T", "reason": "r", "weight": "x"})),
            "weight must be an integer"
        );
        assert_eq!(
            create(json!({"title": "T", "reason": "r", "weight": 1.5})),
            "weight must be an integer"
        );
        assert_eq!(
            create(json!({"title": "T", "reason": "r", "state": "weird"})),
            "initiative state \"weird\" is not open or closed"
        );
        assert_eq!(
            create(json!({"title": "!!!", "reason": "r"})),
            "initiative title does not yield a slug"
        );
        assert_eq!(
            create(json!({"title": "T", "reason": "r", "body": 1, "weight": "x"})),
            "body must be a string"
        );

        let list = |v| refused(initiative_list_op(&built, v));
        assert_eq!(list(json!({"limit": "x"})), "limit must be an integer");
        assert_eq!(list(json!({"offset": []})), "offset must be an integer");

        let update = |v| refused(initiative_update_op(&built, v));
        assert_eq!(update(json!({"reason": "r"})), "slug is required");
        assert_eq!(update(json!({"slug": "s"})), "reason is required");
        assert_eq!(
            update(json!({"slug": "s", "reason": "r", "title": 1})),
            "title must be a string"
        );
        assert_eq!(
            update(json!({"slug": "s", "reason": "r", "body": 1})),
            "body must be a string"
        );
        assert_eq!(
            update(json!({"slug": "s", "reason": "r", "weight": "x"})),
            "weight must be an integer"
        );
        assert_eq!(
            update(json!({"slug": "s", "reason": "r", "state": 1})),
            "state must be a string"
        );
        assert_eq!(
            update(json!({"slug": "s", "reason": "r", "state": "weird"})),
            "initiative state \"weird\" is not open or closed"
        );
        assert_eq!(
            update(json!({"slug": "s", "reason": "r"})),
            "give a title, body, weight or state; there is nothing else to update"
        );
        // null is absent: it does not count as a field to change.
        assert_eq!(
            update(
                json!({"slug": "s", "reason": "r", "title": null, "body": null, "weight": null})
            ),
            "give a title, body, weight or state; there is nothing else to update"
        );
        let publish = |v| refused(initiative_publish_op(&built, v));
        assert_eq!(publish(json!({"reason": "r"})), "slug is required");
        assert_eq!(publish(json!({"slug": "s"})), "reason is required");
    }

    /// Creating an initiative returns it, and listing it back finds the same slug.
    #[test]
    fn creating_an_initiative_lists_it_back() {
        let built = opened();
        let created = initiative_create_op(
            &built,
            json!({"title": "Platform", "reason": "starting the theme"}),
        )
        .unwrap();
        assert_eq!(created["initiative"]["slug"], "platform");
        let listed = initiative_list_op(&built, json!({})).unwrap();
        assert_eq!(listed["initiatives"].as_array().unwrap().len(), 1);
    }

    /// An update that names nothing to change is refused, and so is a slug
    /// that was never created.
    #[test]
    fn an_empty_update_and_an_unknown_slug_are_refused() {
        let built = opened();
        initiative_create_op(
            &built,
            json!({"title": "Platform", "reason": "starting the theme"}),
        )
        .unwrap();
        let empty = initiative_update_op(&built, json!({"slug": "platform", "reason": "nothing"}));
        assert!(
            matches!(empty, Err(VogtError::InvalidRequest(_))),
            "{empty:?}"
        );
        let missing = initiative_update_op(
            &built,
            json!({"slug": "no-such", "title": "x", "reason": "missing"}),
        );
        assert!(
            matches!(missing, Err(VogtError::NotFound(_))),
            "{missing:?}"
        );
    }

    /// Publishing an initiative that spans no linked project writes nothing
    /// upstream and says so with an empty tracking-issue list.
    #[test]
    fn publishing_with_nothing_linked_writes_nothing() {
        let built = opened();
        initiative_create_op(
            &built,
            json!({"title": "Platform", "reason": "starting the theme"}),
        )
        .unwrap();
        let published =
            initiative_publish_op(&built, json!({"slug": "platform", "reason": "ship it"}))
                .unwrap();
        assert_eq!(published["tracking_issues"].as_array().unwrap().len(), 0);
    }
}
