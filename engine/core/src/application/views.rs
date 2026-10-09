//! `why`: why one item is ranked where it is. Ports `why` in
//! `services/views.py`.
//!
//! The score is the same one the ranked views use, so an explanation and the
//! ordering cannot disagree. A declared item is scored from its own fields plus
//! the git signals read off the observed branch and pull-request edges. A
//! reference that names no work item falls back to the observation mirror, the
//! way an observed subject is ranked, and only then is it not found.

use std::sync::Arc;

use crate::application::context::{AppContext, Built};
use crate::application::git_signals::git_signals;
use crate::core::{py_repr, Clock, IdFactory, Moment};
use crate::decisions::{
    priority_of, score_item, title_of, Rankable, RankingInputs, PENDING_INPUTS,
};
use crate::errors::VogtError;
use crate::storage::interface::{DeclaredStore, ObservedStore, ReadView};

/// `why`, as the registry calls it.
pub fn why_op(ctx: &Built, params: serde_json::Value) -> Result<serde_json::Value, VogtError> {
    let reference = params
        .get("ref")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| VogtError::InvalidRequest("why needs a ref".to_string()))?;
    crate::with_ctx!(ctx, |ctx| why(ctx, reference))
}

fn now_of<C: Clock>(clock: &Arc<std::sync::Mutex<C>>) -> Moment {
    clock
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .now()
}

fn why<C: Clock, I: IdFactory>(
    ctx: &AppContext<C, I>,
    reference: &str,
) -> Result<serde_json::Value, VogtError> {
    let now = now_of(&ctx.clock);
    let view = ctx.declared.read()?;
    let (title, rankable, inputs) = match view.work_item_by_ref(reference)? {
        Some(item) => declared(ctx, &view, &item, now)?,
        None => match observation_for(ctx, reference)? {
            Some(observation) => observed(ctx, &observation, now)?,
            None => {
                return Err(VogtError::NotFound(format!(
                    "no work item or observed subject {}",
                    py_repr(reference)
                )));
            }
        },
    };
    let score = score_item(&rankable, &inputs);
    Ok(serde_json::json!({
        "ref": rankable.reference,
        "title": title,
        "total": score.total,
        "contributions": score.contributions.iter().map(|entry| serde_json::json!({
            "input": entry.input,
            "detail": entry.detail,
            "value": entry.value,
            "weight": entry.weight,
            "contribution": entry.contribution,
        })).collect::<Vec<_>>(),
        "inputs_not_yet_available": PENDING_INPUTS.iter()
            .map(|(name, note)| ((*name).to_string(), serde_json::Value::String((*note).to_string())))
            .collect::<serde_json::Map<String, serde_json::Value>>(),
    }))
}

fn declared<C: Clock, I: IdFactory>(
    ctx: &AppContext<C, I>,
    view: &impl ReadView,
    item: &crate::core::WorkItem,
    now: Moment,
) -> Result<(String, Rankable, RankingInputs), VogtError> {
    let fan_out = view.blocking_fan_out(std::slice::from_ref(&item.id))?;
    let weight = match &item.initiative_id {
        Some(id) => view
            .initiative_by_id(id)?
            .map(|found| found.weight)
            .unwrap_or(0),
        None => 0,
    };
    let (open_pr, branch_activity) =
        git_signals(ctx, item.project_id.as_deref(), now)?.for_ref(&item.reference);
    let mut inputs = RankingInputs::at(now);
    inputs.blocking_fan_out = fan_out.get(&item.id).copied().unwrap_or(0);
    inputs.initiative_weight = weight;
    inputs.is_terminal = crate::core::TERMINAL_STATES.contains(&item.state.as_str());
    inputs.open_pr = open_pr;
    inputs.branch_activity_seconds = branch_activity;
    Ok((item.title.clone(), Rankable::from_work_item(item), inputs))
}

fn observed<C: Clock, I: IdFactory>(
    ctx: &AppContext<C, I>,
    observation: &crate::core::Observation,
    now: Moment,
) -> Result<(String, Rankable, RankingInputs), VogtError> {
    let confirmed = ctx
        .observed
        .last_confirmed(std::slice::from_ref(&observation.subject_key))?;
    let (open_pr, branch_activity) =
        git_signals(ctx, observation.project_id.as_deref(), now)?.for_ref(&observation.subject_key);
    let mut inputs = RankingInputs::at(now);
    inputs.open_pr = open_pr;
    inputs.branch_activity_seconds = branch_activity;
    let rankable = Rankable {
        id: observation.subject_key.clone(),
        reference: observation.subject_key.clone(),
        priority: priority_of(observation).to_string(),
        updated_at: observation.observed_at,
        trust_state: trust_for(
            ctx,
            observation.observed_at,
            confirmed.get(&observation.subject_key).copied(),
        ),
        state: "observed".to_string(),
        has_initiative: false,
    };
    Ok((title_of(observation), rankable, inputs))
}

/// `verified` inside the horizon, `stale` once the confirmation ages out. A
/// subject nothing has confirmed takes its first-seen time.
fn trust_for<C: Clock, I: IdFactory>(
    ctx: &AppContext<C, I>,
    observed_at: Moment,
    confirmed_at: Option<Moment>,
) -> String {
    let reference = confirmed_at
        .filter(|confirmed| *confirmed >= observed_at)
        .unwrap_or(observed_at);
    let horizon = ctx.config.verify_horizon_hours * 3600;
    if now_of(&ctx.clock).unix_seconds() - reference.unix_seconds() <= horizon {
        "verified"
    } else {
        "stale"
    }
    .to_string()
}

fn observation_for<C: Clock, I: IdFactory>(
    ctx: &AppContext<C, I>,
    reference: &str,
) -> Result<Option<crate::core::Observation>, VogtError> {
    if !ctx.observed.has_evidence_tables()? {
        return Ok(None);
    }
    Ok(ctx
        .observed
        .list_observations(None, None, Some(reference), 1, 0)?
        .into_iter()
        .next())
}
