//! Decisions a person makes for an agent (WI-1321). Ports
//! `application/services/decisions.py`.
//!
//! An agent — usually an overseer — puts a question to a person with two to
//! four options, one recommended; a person answers it from the Inbox. The
//! record is shaped like a session grant, so the Inbox and the person-only rule
//! are reused.
//!
//! Three rules shape this module:
//!
//! - Only a person answers. `answer` refuses every agent principal before
//!   anything else.
//! - Nothing is applied. A decision is not a grant and not a gate: the asker
//!   reads the answer and acts through the op that checks the action.
//! - Expiry implies no answer. A pending decision past `expires_at` reads
//!   `expired`, computed on read; it cannot be answered, and there is no
//!   background job.

use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use crate::application::context::{write_of, AppContext, Built};
use crate::application::grants::own_engine_session;
use crate::application::services::now_of;
use crate::application::writes::{audited_write, validate_reason, WriteOutcome};
use crate::core::{
    Clock, Decision, DecisionAnswer, DecisionOption, DecisionState, IdFactory, Moment,
};
use crate::errors::VogtError;
use crate::storage::interface::{DeclaredStore, ReadView, WriteTxn};

const DECISION_ASKED_EVENT: &str = "decision.asked";
const DECISION_ANSWERED_EVENT: &str = "decision.answered";
const DECISION_WITHDRAWN_EVENT: &str = "decision.withdrawn";
const DEFAULT_TTL_SECONDS: i64 = 86_400;
/// Python's `ReadView.list_decisions` default: the Inbox reads at most this many.
const INBOX_PENDING_LIMIT: i64 = 200;

pub fn ask_decision_op(built: &Built, params: Value) -> Result<Value, VogtError> {
    crate::with_ctx!(built, |ctx| ask(ctx, &params))
}

pub fn answer_decision_op(built: &Built, params: Value) -> Result<Value, VogtError> {
    crate::with_ctx!(built, |ctx| answer(ctx, &params))
}

pub fn withdraw_decision_op(built: &Built, params: Value) -> Result<Value, VogtError> {
    crate::with_ctx!(built, |ctx| withdraw(ctx, &params))
}

pub fn list_decisions_op(built: &Built, params: Value) -> Result<Value, VogtError> {
    crate::with_ctx!(built, |ctx| list(ctx, &params))
}

/// Record a question for a person; the Inbox shows it until it is answered.
fn ask<C: Clock + 'static, I: IdFactory + 'static>(
    ctx: &AppContext<C, I>,
    params: &Value,
) -> Result<Value, VogtError> {
    let reason = validate_reason(&required(params, "decision.ask", "reason")?)?;
    let question = required(params, "decision.ask", "question")?
        .trim()
        .to_string();
    if question.is_empty() {
        return Err(VogtError::InvalidRequest(
            "a decision asks a question".to_string(),
        ));
    }
    let options = options_of(params)?;
    let context = text(params, "context").trim().to_string();
    let ttl_seconds = params
        .get("ttl_seconds")
        .and_then(Value::as_i64)
        .unwrap_or(DEFAULT_TTL_SECONDS);
    let work_item_id = match text_opt(params, "work_item") {
        Some(reference) => Some(
            ctx.declared
                .read()?
                .work_item_by_ref(reference.trim())?
                .ok_or_else(|| {
                    VogtError::NotFound(format!(
                        "no work item {}",
                        crate::core::py_repr(&reference)
                    ))
                })?
                .id,
        ),
        None => None,
    };
    // The asker's session is the token's, never a parameter.
    let asker_session = if ctx.principal.kind == crate::core::ActorKind::Agent {
        own_engine_session(ctx)?
    } else {
        None
    };

    let mut write = write_of(ctx);
    let clock = Arc::clone(write.clock());
    let ids = Arc::clone(write.ids());
    audited_write(&mut write, "decision.ask", &reason, |txn, actor| {
        let now = now_of(&clock);
        let decision = Decision {
            id: ids
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .next("dec"),
            asker_actor_id: actor.id.clone(),
            asker_session: asker_session.clone(),
            work_item_id: work_item_id.clone(),
            question: question.clone(),
            options: options.clone(),
            context: context.clone(),
            state: DecisionState::Pending,
            answer: None,
            answered_by_actor_id: None,
            created_at: now,
            answered_at: None,
            expires_at: plus_seconds(now, ttl_seconds),
            reason: reason.clone(),
        };
        txn.insert_decision(&decision)?;
        outcome(&*txn, &decision, DECISION_ASKED_EVENT, &clock)
    })
}

/// Answer a pending decision with one of its options. A person only.
fn answer<C: Clock + 'static, I: IdFactory + 'static>(
    ctx: &AppContext<C, I>,
    params: &Value,
) -> Result<Value, VogtError> {
    let reason = validate_reason(&required(params, "decision.answer", "reason")?)?;
    if ctx.principal.kind == crate::core::ActorKind::Agent {
        return Err(VogtError::PersonRequired(format!(
            "only a person answers a decision, not an agent ({}); leave it for the Inbox",
            ctx.principal.identity_ref
        )));
    }
    let id = required(params, "decision.answer", "id")?;
    let decision = load(ctx, &id)?;
    let label = required(params, "decision.answer", "label")?
        .trim()
        .to_string();
    if !decision.options.iter().any(|option| option.label == label) {
        let offered = decision
            .options
            .iter()
            .map(|option| crate::core::py_repr(&option.label))
            .collect::<Vec<_>>()
            .join(", ");
        return Err(VogtError::InvalidRequest(format!(
            "{} is not an option of {}; it offers {offered}",
            crate::core::py_repr(&label),
            decision.id
        )));
    }
    let note = text(params, "note").trim().to_string();

    let mut write = write_of(ctx);
    let clock = Arc::clone(write.clock());
    audited_write(&mut write, "decision.answer", &reason, |txn, actor| {
        // Two reads, as Python makes: one to test pending, one to stamp the
        // answer. The step clock hands out a fresh moment per read.
        let current = pending(&*txn, &decision.id, now_of(&clock))?;
        let now = now_of(&clock);
        let answered = Decision {
            state: DecisionState::Answered,
            answer: Some(DecisionAnswer {
                label: label.clone(),
                note: note.clone(),
            }),
            answered_by_actor_id: Some(actor.id.clone()),
            answered_at: Some(now),
            ..current
        };
        txn.update_decision(&answered)?;
        outcome(&*txn, &answered, DECISION_ANSWERED_EVENT, &clock)
    })
}

/// Withdraw a pending decision: a person, or the session that asked.
fn withdraw<C: Clock + 'static, I: IdFactory + 'static>(
    ctx: &AppContext<C, I>,
    params: &Value,
) -> Result<Value, VogtError> {
    let reason = validate_reason(&required(params, "decision.withdraw", "reason")?)?;
    let id = required(params, "decision.withdraw", "id")?;
    let decision = load(ctx, &id)?;
    if ctx.principal.kind == crate::core::ActorKind::Agent && !is_asker(ctx, &decision)? {
        return Err(VogtError::GrantRefused(format!(
            "only a person, or the session that asked, withdraws a decision ({} did neither)",
            ctx.principal.identity_ref
        )));
    }

    let mut write = write_of(ctx);
    let clock = Arc::clone(write.clock());
    audited_write(&mut write, "decision.withdraw", &reason, |txn, _actor| {
        let current = pending(&*txn, &decision.id, now_of(&clock))?;
        let withdrawn = Decision {
            state: DecisionState::Withdrawn,
            ..current
        };
        txn.update_decision(&withdrawn)?;
        outcome(&*txn, &withdrawn, DECISION_WITHDRAWN_EVENT, &clock)
    })
}

/// Decisions, newest first, by state, work item or asker.
fn list<C: Clock + 'static, I: IdFactory + 'static>(
    ctx: &AppContext<C, I>,
    params: &Value,
) -> Result<Value, VogtError> {
    let state = text_opt(params, "state");
    let limit = params.get("limit").and_then(Value::as_i64).unwrap_or(50);
    // `expired` is not stored: it is a pending row past its expiry.
    let stored_state = match state.as_deref() {
        Some("expired") => Some("pending"),
        other => other,
    };
    let view = ctx.declared.read()?;
    let work_item_id = match text_opt(params, "work_item") {
        Some(reference) => Some(
            view.work_item_by_ref(reference.trim())?
                .ok_or_else(|| {
                    VogtError::NotFound(format!(
                        "no work item {}",
                        crate::core::py_repr(&reference)
                    ))
                })?
                .id,
        ),
        None => None,
    };
    let asker_id = match text_opt(params, "asker") {
        Some(asker) => match view.actor_by_identity(asker.trim())? {
            Some(actor) => Some(actor.id),
            None => return Ok(json!({ "decisions": [] })),
        },
        None => None,
    };
    let rows = view.list_decisions(
        stored_state,
        work_item_id.as_deref(),
        asker_id.as_deref(),
        limit,
    )?;
    // One clock read per row, before the state filter, as Python's list.
    let mut views = Vec::new();
    for row in &rows {
        views.push(view_of(&view, row, now_of(&ctx.clock))?);
    }
    if matches!(state.as_deref(), Some("pending" | "expired")) {
        views.retain(|found| found["state"].as_str() == state.as_deref());
    }
    Ok(json!({ "decisions": views }))
}

/// What the Inbox shows: every decision still waiting for a person, newest
/// first. Ports `pending_decisions` in the Python service.
pub fn pending_decisions<C: Clock>(
    view: &impl ReadView,
    clock: &Arc<Mutex<C>>,
) -> Result<Vec<Decision>, VogtError> {
    let mut pending = Vec::new();
    for decision in view.list_decisions(Some("pending"), None, None, INBOX_PENDING_LIMIT)? {
        if decision.effective_state(now_of(clock)) == "pending" {
            pending.push(decision);
        }
    }
    Ok(pending)
}

// -- helpers -----------------------------------------------------------------

fn is_asker<C: Clock, I: IdFactory>(
    ctx: &AppContext<C, I>,
    decision: &Decision,
) -> Result<bool, VogtError> {
    let caller = ctx
        .declared
        .read()?
        .actor_by_identity(&ctx.principal.identity_ref)?;
    if caller.is_some_and(|actor| actor.id == decision.asker_actor_id) {
        return Ok(true);
    }
    let own = own_engine_session(ctx)?;
    Ok(own.is_some() && own == decision.asker_session)
}

fn load<C: Clock, I: IdFactory>(ctx: &AppContext<C, I>, id: &str) -> Result<Decision, VogtError> {
    let found = ctx.declared.read()?.decision(id.trim())?;
    found.ok_or_else(|| VogtError::NotFound(format!("no decision {}", crate::core::py_repr(id))))
}

/// The stored decision, if it is still pending at `now`. Anything else is a
/// conflict, and the message names the state the decision is in.
fn pending(txn: &impl ReadView, id: &str, now: Moment) -> Result<Decision, VogtError> {
    let current = txn
        .decision(id)?
        .ok_or_else(|| VogtError::NotFound(format!("no decision {}", crate::core::py_repr(id))))?;
    let state = current.effective_state(now);
    if state != "pending" {
        return Err(VogtError::Conflict(format!(
            "decision {} is {state}, not pending",
            current.id
        )));
    }
    Ok(current)
}

/// The write's outcome. Reads the clock for the view, as Python's `_view` does:
/// the step clock advances per read, so the count is part of the parity.
fn outcome<C: Clock>(
    txn: &impl ReadView,
    decision: &Decision,
    event: &str,
    clock: &Arc<Mutex<C>>,
) -> Result<WriteOutcome<Value>, VogtError> {
    Ok(WriteOutcome {
        result: json!({ "decision": view_of(txn, decision, now_of(clock))? }),
        entity_kind: "decision".to_string(),
        entity_id: decision.id.clone(),
        payload: serde_json::to_value(decision).unwrap_or(Value::Null),
        event_kind: event.to_string(),
        summary: summary_of(txn, decision)?,
    })
}

/// A decision as callers read it. Ports `_view` in the Python service.
fn view_of(view: &impl ReadView, decision: &Decision, now: Moment) -> Result<Value, VogtError> {
    Ok(json!({
        "id": decision.id,
        "asker": identity_of(view, &decision.asker_actor_id)?,
        "asker_session": decision.asker_session,
        "work_item": work_ref_of(view, decision.work_item_id.as_deref())?,
        "question": decision.question,
        "options": decision.options.iter().map(option_json).collect::<Vec<_>>(),
        "context": decision.context,
        "state": decision.effective_state(now),
        "answer": decision.answer.as_ref().map(|answer| json!({
            "label": answer.label,
            "note": answer.note,
        })),
        "answered_by": match decision.answered_by_actor_id.as_deref() {
            Some(id) => Some(identity_of(view, id)?),
            None => None,
        },
        "created_at": decision.created_at.to_json(),
        "answered_at": decision.answered_at.map(|moment| moment.to_json()),
        "expires_at": decision.expires_at.to_json(),
        "reason": decision.reason,
    }))
}

fn option_json(option: &DecisionOption) -> Value {
    json!({
        "label": option.label,
        "description": option.description,
        "recommended": option.recommended,
    })
}

fn summary_of(view: &impl ReadView, decision: &Decision) -> Result<Value, VogtError> {
    Ok(json!({
        "decision_id": decision.id,
        "asker_session": decision.asker_session,
        "work_item": work_ref_of(view, decision.work_item_id.as_deref())?,
        "question": decision.question.chars().take(200).collect::<String>(),
        "state": decision.state.to_string(),
        "answer": decision.answer.as_ref().map(|answer| answer.label.clone()),
    }))
}

fn identity_of(view: &impl ReadView, actor_id: &str) -> Result<String, VogtError> {
    Ok(view
        .actor_by_id(actor_id)?
        .map_or_else(|| actor_id.to_string(), |actor| actor.identity_ref))
}

fn work_ref_of(
    view: &impl ReadView,
    work_item_id: Option<&str>,
) -> Result<Option<String>, VogtError> {
    match work_item_id {
        None => Ok(None),
        Some(id) => Ok(Some(
            view.work_item_by_id(id)?
                .map_or_else(|| id.to_string(), |item| item.reference),
        )),
    }
}

/// `options` as the caller wrote them: two to four, labelled, exactly one
/// recommended, labels distinct.
fn options_of(params: &Value) -> Result<Vec<DecisionOption>, VogtError> {
    let raw = params
        .get("options")
        .cloned()
        .ok_or_else(|| VogtError::InvalidRequest("decision.ask needs options".to_string()))?;
    let parsed: Vec<DecisionOption> = serde_json::from_value(raw)
        .map_err(|err| VogtError::InvalidRequest(format!("options: {err}")))?;
    let options: Vec<DecisionOption> = parsed
        .into_iter()
        .map(|option| DecisionOption {
            label: option.label.trim().to_string(),
            description: option.description.trim().to_string(),
            recommended: option.recommended,
        })
        .collect();
    if options.iter().any(|option| option.label.is_empty()) {
        return Err(VogtError::InvalidRequest(
            "every option has a label".to_string(),
        ));
    }
    let mut labels: Vec<&str> = options.iter().map(|option| option.label.as_str()).collect();
    labels.sort_unstable();
    labels.dedup();
    if labels.len() != options.len() {
        return Err(VogtError::InvalidRequest(
            "option labels must differ".to_string(),
        ));
    }
    if options.iter().filter(|option| option.recommended).count() != 1 {
        return Err(VogtError::InvalidRequest(
            "exactly one option is recommended".to_string(),
        ));
    }
    Ok(options)
}

fn plus_seconds(moment: Moment, seconds: i64) -> Moment {
    Moment::from_unix(
        moment.unix_seconds().saturating_add(seconds),
        moment.nanos(),
    )
}

fn required(params: &Value, operation: &str, name: &str) -> Result<String, VogtError> {
    params
        .get(name)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| VogtError::InvalidRequest(format!("{operation} needs a {name}")))
}

fn text(params: &Value, name: &str) -> String {
    text_opt(params, name).unwrap_or_default()
}

fn text_opt(params: &Value, name: &str) -> Option<String> {
    params.get(name).and_then(Value::as_str).map(str::to_string)
}
