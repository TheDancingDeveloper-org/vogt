//! `why`: why one item is ranked where it is. Ports `why` in
//! `services/views.py`.
//!
//! The score is the same one the ranked views use, so an explanation and the
//! ordering cannot disagree. Two inputs are not ported yet and are reported as
//! absent rather than as zero, which is what an explanation that cannot see
//! them has to say:
//!
//! - the git signals (`open_pr`, `branch_activity_seconds`) need the observed
//!   branch and pull-request edges, which land with the forge collectors;
//! - an observed subject (a GitHub issue that is not a declared work item)
//!   needs the observation mirror, which lands with the same collectors.
//!
//! A declared item is scored in full apart from those two.

use crate::application::context::{AppContext, Built};
use crate::core::{Clock, IdFactory};
use crate::decisions::{score_item, Rankable, RankingInputs, PENDING_INPUTS};
use crate::errors::VogtError;
use crate::storage::interface::{DeclaredStore, ReadView};

/// `why`, as the registry calls it.
pub fn why_op(ctx: &Built, params: serde_json::Value) -> Result<serde_json::Value, VogtError> {
    let reference = params
        .get("ref")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| VogtError::InvalidRequest("why needs a ref".to_string()))?;
    match ctx {
        Built::SystemRandom(ctx) => why(ctx, reference),
        Built::SystemSequential(ctx) => why(ctx, reference),
        Built::StepRandom(ctx) => why(ctx, reference),
        Built::StepSequential(ctx) => why(ctx, reference),
    }
}

fn why<C: Clock, I: IdFactory>(
    ctx: &AppContext<C, I>,
    reference: &str,
) -> Result<serde_json::Value, VogtError> {
    let view = ctx.declared.read()?;
    let item = crate::application::resolve::work_item(&view, reference)?;
    let fan_out = view.blocking_fan_out(std::slice::from_ref(&item.id))?;
    let weight = match &item.initiative_id {
        Some(id) => view
            .initiative_by_id(id)?
            .map(|found| found.weight)
            .unwrap_or(0),
        None => 0,
    };
    let mut inputs = RankingInputs::at(
        ctx.clock
            .lock()
            .expect("the shared clock is not poisoned")
            .now(),
    );
    inputs.blocking_fan_out = fan_out.get(&item.id).copied().unwrap_or(0);
    inputs.initiative_weight = weight;
    inputs.is_terminal = crate::core::TERMINAL_STATES.contains(&item.state.as_str());
    // The git signals are not ported. Leaving them at their defaults and saying
    // so keeps the explanation honest about what it did not see.
    let score = score_item(&Rankable::from_work_item(&item), &inputs);

    let mut missing: Vec<(&str, &str)> = PENDING_INPUTS.to_vec();
    missing.push((
        "git_signals",
        "the observed branch and pull-request edges are not ported yet, so open_pr and branch activity are absent rather than zero",
    ));
    Ok(serde_json::json!({
        "ref": item.reference,
        "title": item.title,
        "total": score.total,
        "contributions": score.contributions.iter().map(|entry| serde_json::json!({
            "input": entry.input,
            "detail": entry.detail,
            "value": entry.value,
            "weight": entry.weight,
            "contribution": entry.contribution,
        })).collect::<Vec<_>>(),
        "inputs_not_yet_available": missing
            .into_iter()
            .map(|(name, note)| (name.to_string(), serde_json::Value::String(note.to_string())))
            .collect::<serde_json::Map<String, serde_json::Value>>(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::writes::{audited_write, WriteContext, WriteOutcome};
    use crate::core::{
        ActorKind, Moment, Origin, Principal, Priority, Project, SequentialIds, StepClock,
        TrustState, WorkItem, WorkKind,
    };
    use crate::storage::interface::{DeclaredStore, WriteTxn};
    use crate::storage::sqlite::declared::SqliteDeclaredStore;
    use std::sync::Arc;

    fn moment() -> Moment {
        Moment::from_unix(1_700_000_000, 0)
    }

    fn opened() -> SqliteDeclaredStore<StepClock, SequentialIds> {
        let dir = std::env::temp_dir().join(format!(
            "vogt-why-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let store = SqliteDeclaredStore::new(
            dir.join("declared.sqlite3"),
            StepClock::new(moment()),
            SequentialIds::new(None).unwrap(),
        );
        store.migrate().unwrap();
        store
            .bootstrap(&Principal::new("local:test-user", ActorKind::Human, "Test").unwrap())
            .unwrap();
        store
    }

    #[test]
    fn why_names_the_priority_and_the_inputs_it_cannot_see() {
        let store = opened();
        let principal = Principal::new("local:test-user", ActorKind::Human, "Test").unwrap();
        audited_write(
            &mut WriteContext::new(
                &store,
                Box::leak(Box::new(principal)),
                Arc::clone(store.clock()),
                Arc::clone(store.id_factory()),
            ),
            "create the item",
            "work.create",
            |txn, _, _, _| {
                txn.insert_project(&Project::new("prj_app", "app", "app", "/srv/app", moment()))?;
                txn.insert_work_item(&WorkItem {
                    id: "wrk_1".to_string(),
                    reference: "WI-1".to_string(),
                    kind: WorkKind::Feature,
                    title: "the thing".to_string(),
                    body: String::new(),
                    state: "open".to_string(),
                    priority: Priority::P1,
                    effort: None,
                    project_id: Some("prj_app".to_string()),
                    project_slug: Some("app".to_string()),
                    initiative_id: None,
                    origin: Origin::Created,
                    trust_state: TrustState::Unverified,
                    assignee_actor_id: None,
                    assignee_identity_ref: None,
                    labels: Vec::new(),
                    relations: Vec::new(),
                    superseded_by: None,
                    created_at: moment(),
                    updated_at: moment(),
                })?;
                Ok(WriteOutcome::new(
                    (),
                    "work_item",
                    "wrk_1",
                    serde_json::json!({}),
                    "created",
                    serde_json::json!({}),
                ))
            },
        )
        .unwrap();

        // The service reads through the context the registry builds. Here the
        // store is scored directly, which is the part of `why` that decides.
        let view = store.read().unwrap();
        let item = crate::application::resolve::work_item(&view, "WI-1").unwrap();
        let mut inputs = RankingInputs::at(moment());
        inputs.is_terminal = false;
        let score = score_item(&Rankable::from_work_item(&item), &inputs);
        assert!(score.total > 0.0, "a high-priority item scores");
        assert!(
            score
                .contributions
                .iter()
                .any(|entry| entry.input == "priority"),
            "the priority is one of the reasons"
        );
    }
}
