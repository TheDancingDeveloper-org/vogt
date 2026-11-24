//! `work.land` (WI-1328): land a reviewed branch onto the project's base.
//! Ports `land_work` from `src/vogt/application/services/work.py`.
//!
//! The gate that decides what merges. A person or an oversight session asks;
//! the project's merges must be oversight's; the latest hand-off names the
//! branch; and the latest verdict is `no_blockers` for exactly `head_sha`,
//! from someone other than the worker. A reviewer session's provenance is
//! re-read from the session store, never taken from the comment, so the land
//! does not rest on the comment gate alone. The engine then lands with the
//! core's own credential and its refusal is surfaced verbatim. Every refusal
//! is audited with the rule it broke; the diff never is.

use std::collections::{BTreeSet, HashMap};

use serde_json::{json, Value};

use crate::adapters::engine::EngineClient;
use crate::application::context::{write_of, AppContext, Built};
use crate::application::models::views::{VerdictFields, VerdictFieldsVerdict};
use crate::application::resolve;
use crate::application::services::sessions::{answers_as_person, engine_of};
use crate::application::session_commit::caller_engine_session_id;
use crate::application::writes::{audited_action, audited_write, validate_reason, WriteOutcome};
use crate::core::{Clock, CodingSession, Comment, IdFactory, WorkItem};
use crate::errors::VogtError;
use crate::storage::interface::{DeclaredStore, ReadView, WriteTxn};

const WORK_LAND: &str = "work.land";
const WORK_LANDED_EVENT: &str = "work.landed";
const WORK_LAND_REFUSED_EVENT: &str = "work.land_refused";

pub fn work_land_op(built: &Built, params: Value) -> Result<Value, VogtError> {
    crate::with_ctx!(built, |ctx| land_work(ctx, params))
}

/// The landing rule, read off the project's agent policy.
struct Landing {
    merge_by: String,
    base_branch: String,
    merge_mode: String,
    linear_history: bool,
}

fn landing_of(policy: Option<&Value>) -> Option<Landing> {
    let policy = policy.filter(|value| value.is_object())?;
    let text = |name: &str| {
        policy
            .get(name)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    Some(Landing {
        merge_by: text("merge_by"),
        base_branch: text("base_branch"),
        merge_mode: text("merge_mode"),
        linear_history: policy
            .get("linear_history")
            .and_then(Value::as_bool)
            .unwrap_or(true),
    })
}

fn verdict_of(comment: &Comment) -> Option<VerdictFields> {
    comment
        .verdict
        .as_ref()
        .and_then(|value| serde_json::from_value(value.clone()).ok())
}

fn text_field(fields: &Value, name: &str) -> Option<String> {
    fields
        .get(name)
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}

pub(crate) fn land_work<C, I>(ctx: &AppContext<C, I>, params: Value) -> Result<Value, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let reference = params
        .get("ref")
        .and_then(Value::as_str)
        .ok_or_else(|| VogtError::InvalidRequest("ref is required".into()))?
        .to_string();
    let reason = validate_reason(params.get("reason").and_then(Value::as_str).unwrap_or(""))?;
    let head_sha = params
        .get("head_sha")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    let engine = engine_of(ctx)?;
    let view = ctx.declared.read()?;
    let item = resolve::work_item(&view, &reference)?;
    let project = item
        .project_id
        .as_deref()
        .map(|id| view.project_by_id(id))
        .transpose()?
        .flatten();
    let thread = view.comments_for(&item.id, 500, true)?;
    let mut reviewers: HashMap<String, Option<CodingSession>> = HashMap::new();
    for comment in &thread {
        if comment.kind != "verdict" {
            continue;
        }
        if let Some(session) = verdict_of(comment)
            .and_then(|verdict| verdict.reviewer_session)
            .filter(|session| !session.is_empty())
        {
            let found = view.session_by_engine_id(&session)?;
            reviewers.insert(session, found);
        }
    }
    drop(view);

    let refuse = |rule: &str| -> VogtError {
        let mut writing = write_of(ctx);
        if let Err(error) = audited_action(
            &mut writing,
            WORK_LAND,
            &reason,
            "work_item",
            &item.id,
            &json!({"ref": item.reference, "head_sha": head_sha, "refused": rule}),
            WORK_LAND_REFUSED_EVENT,
            None,
        ) {
            return error;
        }
        VogtError::LandRefused(format!("{} not landed: {rule}", item.reference))
    };

    if let Some(rule) = land_caller_refusal(ctx, engine)? {
        return Err(refuse(&rule));
    }
    let landing = project
        .as_ref()
        .and_then(|project| landing_of(project.agent_policy.landing.as_ref()));
    let (Some(project), Some(landing)) = (project.as_ref(), landing) else {
        return Err(refuse("this project's merges are the operator's"));
    };
    if landing.merge_by != "oversight" {
        return Err(refuse("this project's merges are the operator's"));
    }
    let handoffs: Vec<&Value> = thread
        .iter()
        .filter(|comment| comment.kind == "handoff")
        .filter_map(|comment| comment.handoff.as_ref())
        .collect();
    let Some(branch) = handoffs
        .iter()
        .find_map(|fields| text_field(fields, "branch"))
    else {
        return Err(refuse("no hand-off names a branch"));
    };
    let Some(verdict) = thread
        .iter()
        .find(|comment| comment.kind == "verdict")
        .and_then(verdict_of)
    else {
        return Err(refuse("there is no review verdict"));
    };
    let workers: BTreeSet<String> = handoffs
        .iter()
        .filter_map(|fields| text_field(fields, "session"))
        .collect();
    let reviewer = reviewers
        .get(verdict.reviewer_session.as_deref().unwrap_or(""))
        .and_then(Option::as_ref);
    if let Some(rule) = verdict_refusal(&verdict, &head_sha, &item, &workers, reviewer) {
        return Err(refuse(&rule));
    }

    let landed = engine.land(
        &project.root_path,
        &branch,
        &head_sha,
        &landing.base_branch,
        &landing.merge_mode,
        &project.agent_policy.forbidden_text,
        landing.linear_history,
    )?;
    let sha = |name: &str| match landed.get(name) {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(text)) => text.clone(),
        Some(other) => other.to_string(),
    };
    let onto_before = sha("onto_before");
    let onto_after = sha("onto_after");
    let commits: Vec<String> = landed
        .get("commits")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .map(|item| match item {
                    Value::String(text) => text.clone(),
                    other => other.to_string(),
                })
                .collect()
        })
        .unwrap_or_default();
    let rebased = landed.get("rebased").is_some_and(truthy);
    let result = json!({
        "onto_before": onto_before,
        "onto_after": onto_after,
        "commits": commits,
        "rebased": rebased,
    });
    let by = verdict
        .reviewer_session
        .clone()
        .unwrap_or_else(|| "a person".to_string());
    let note = format!(
        "landed {onto_after} onto {} ({} commits; verdict by {by})",
        landing.base_branch,
        commits.len()
    );
    let mut writing = write_of(ctx);
    let clock = std::sync::Arc::clone(writing.clock());
    let ids = std::sync::Arc::clone(writing.ids());
    let item_id = item.id.clone();
    let item_ref = item.reference.clone();
    let onto = landing.base_branch.clone();
    let reviewer_session = verdict.reviewer_session.clone();
    audited_write(&mut writing, WORK_LAND, &reason, move |txn, actor| {
        let comment = Comment {
            id: crate::application::services::next_id(&ids, "cmt"),
            work_item_id: item_id.clone(),
            actor_id: actor.id.clone(),
            actor_display_name: actor.display_name.clone(),
            body: note,
            created_at: crate::application::services::now_of(&clock),
            kind: "note".to_string(),
            handoff: None,
            verdict: None,
            verification: None,
        };
        txn.insert_comment(&comment)?;
        Ok(WriteOutcome::new(
            result.clone(),
            "work_item",
            &item_id,
            json!({
                "ref": item_ref,
                "branch": branch,
                "head_sha": head_sha,
                "onto": onto,
                "onto_before": onto_before,
                "onto_after": onto_after,
                "commits": commits.len(),
                "rebased": rebased,
                "actor": actor.identity_ref,
                "reviewer_session": reviewer_session,
                "comment_id": comment.id,
            }),
            WORK_LANDED_EVENT,
            json!({
                "ref": item_ref,
                "branch": branch,
                "onto": onto,
                "onto_after": onto_after,
                "reviewer_session": reviewer_session,
            }),
        ))
    })
}

/// Python's `bool(...)` of a JSON value.
fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(flag) => *flag,
        Value::Number(number) => number.as_f64().is_some_and(|n| n != 0.0),
        Value::String(text) => !text.is_empty(),
        Value::Array(items) => !items.is_empty(),
        Value::Object(map) => !map.is_empty(),
    }
}

/// Who may land (operator decision 1, 2026-10-13): a person, or a session
/// whose engine role is `oversight`. The role is read from the engine.
fn land_caller_refusal<C, I>(
    ctx: &AppContext<C, I>,
    engine: &EngineClient,
) -> Result<Option<String>, VogtError>
where
    C: Clock,
    I: IdFactory,
{
    if answers_as_person(ctx)? {
        return Ok(None);
    }
    let Some(caller_id) = caller_engine_session_id(ctx)? else {
        return Ok(Some(format!(
            "only a person or an oversight session lands a branch ({} is neither)",
            ctx.principal.identity_ref
        )));
    };
    let role = engine
        .get_session(&caller_id)?
        .map_or_else(|| "unknown".to_string(), |session| session.role);
    if role != "oversight" {
        return Ok(Some(format!(
            "only a person or an oversight session lands a branch ({caller_id} is {role})"
        )));
    }
    Ok(None)
}

/// Why the latest verdict does not license this land, or none.
fn verdict_refusal(
    verdict: &VerdictFields,
    head_sha: &str,
    item: &WorkItem,
    workers: &BTreeSet<String>,
    reviewer: Option<&CodingSession>,
) -> Option<String> {
    if verdict.verdict != VerdictFieldsVerdict::NoBlockers {
        return Some("the latest verdict has blockers".to_string());
    }
    if verdict.head_sha != head_sha {
        return Some(format!(
            "the latest verdict is for {}, not {head_sha}; the branch moved since the review",
            verdict.head_sha
        ));
    }
    // A person's verdict: no session to re-check.
    let reviewer_session = verdict.reviewer_session.as_deref()?;
    if workers.contains(reviewer_session) {
        return Some("the verdict is the worker's own".to_string());
    }
    let Some(reviewer) = reviewer else {
        return Some(format!(
            "the verdict's reviewer session {reviewer_session} is not in the session store"
        ));
    };
    if workers.contains(&reviewer.id) || workers.contains(&reviewer.engine_session_id) {
        return Some("the verdict is the worker's own".to_string());
    }
    if reviewer.purpose.as_deref() != Some("review") {
        return Some(format!(
            "the reviewer session {reviewer_session} is not a review"
        ));
    }
    match reviewer.started_by_kind.as_deref() {
        Some("person" | "oversight") => {}
        other => {
            return Some(format!(
                "the reviewer session {reviewer_session} was started by {}",
                other
                    .filter(|text| !text.is_empty())
                    .unwrap_or("an unknown starter")
            ))
        }
    }
    if reviewer.work_item_id.as_deref() != Some(item.id.as_str()) {
        return Some(format!(
            "the reviewer session {reviewer_session} is not bound to {}",
            item.reference
        ));
    }
    None
}
