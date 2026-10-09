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
pub fn trust_for<C: Clock, I: IdFactory>(
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

/// The live suppressions, ready to test subjects against. Ports
/// `SuppressionFilter`: an exact key hides a subject everywhere, a scoped exact
/// key hides it in one project, and a pattern matches the way `fnmatch` does.
struct SuppressionFilter {
    exact: std::collections::HashSet<String>,
    scoped_exact: std::collections::HashSet<(String, String)>,
    patterns: Vec<(String, Option<String>)>,
}

impl SuppressionFilter {
    fn build(suppressions: &[crate::core::Suppression]) -> Self {
        let mut filter = Self {
            exact: std::collections::HashSet::new(),
            scoped_exact: std::collections::HashSet::new(),
            patterns: Vec::new(),
        };
        for entry in suppressions.iter().filter(|entry| entry.active()) {
            if entry.match_kind == crate::core::MatchKind::Pattern {
                filter.patterns.push((
                    entry.subject_key_or_pattern.clone(),
                    entry.scope_project_id.clone(),
                ));
            } else if let Some(scope) = &entry.scope_project_id {
                filter
                    .scoped_exact
                    .insert((scope.clone(), entry.subject_key_or_pattern.clone()));
            } else {
                filter.exact.insert(entry.subject_key_or_pattern.clone());
            }
        }
        filter
    }

    fn hides(&self, subject_key: &str, project_id: Option<&str>) -> bool {
        if self.exact.contains(subject_key) {
            return true;
        }
        if let Some(project_id) = project_id {
            if self
                .scoped_exact
                .contains(&(project_id.to_string(), subject_key.to_string()))
            {
                return true;
            }
        }
        self.patterns.iter().any(|(pattern, scope)| {
            (scope.is_none() || scope.as_deref() == project_id)
                && crate::adapters::forge::glob_match(pattern, subject_key)
        })
    }
}

/// One ranked row, carrying the fields the wire wants and the score's order.
struct RankedRow {
    score: f64,
    reference: String,
    value: serde_json::Value,
}

/// What `_gather` counted and ranked.
struct Gathered {
    ranked: Vec<RankedRow>,
    declared: i64,
    observed: i64,
    suppressed: i64,
    closed: i64,
    excluded_unlinked: i64,
}

struct GatherQuery<'a> {
    project: Option<&'a str>,
    kinds: Vec<String>,
    priorities: Vec<String>,
    assignee: Option<&'a str>,
    initiative: Option<&'a str>,
    label: Option<&'a str>,
    trust_states: Vec<String>,
    include_prs: bool,
}

/// The candidate population of a ranked view, scored and ordered. Ports
/// `_gather`: declared rows first, then the observed subjects that name no
/// declared work, with a PR collapsed under the item it implements.
fn gather<C: Clock, I: IdFactory>(
    ctx: &AppContext<C, I>,
    query: &GatherQuery<'_>,
) -> Result<Gathered, VogtError> {
    let now = now_of(&ctx.clock);
    let view = ctx.declared.read()?;
    let project = query
        .project
        .map(|slug| crate::application::resolve::project(&view, slug))
        .transpose()?;
    let filter = crate::storage::interface::WorkFilter {
        project_id: project.as_ref().map(|project| project.id.clone()),
        kinds: query.kinds.clone(),
        priorities: query.priorities.clone(),
        assignee_actor_id: query
            .assignee
            .map(|identity| {
                crate::application::resolve::actor(&view, identity).map(|actor| actor.id)
            })
            .transpose()?,
        initiative_id: query
            .initiative
            .map(|slug| {
                crate::application::resolve::initiative(&view, slug).map(|initiative| initiative.id)
            })
            .transpose()?,
        label: query.label.map(str::to_string),
        trust_states: query.trust_states.clone(),
        exclude_terminal: true,
        exclude_unlinked_native: project.is_none(),
        ..crate::storage::interface::WorkFilter::default()
    };
    let excluded_unlinked = if project.is_none() {
        let unfiltered = crate::storage::interface::WorkFilter {
            exclude_unlinked_native: false,
            ..filter.clone()
        };
        view.count_work_items(&unfiltered)? - view.count_work_items(&filter)?
    } else {
        0
    };
    let items = view.list_work_items(&filter)?;
    let ids: Vec<String> = items.iter().map(|item| item.id.clone()).collect();
    let fan_out = view.blocking_fan_out(&ids)?;
    let weights = initiative_weights(&view)?;
    let signals = git_signals(
        ctx,
        project.as_ref().map(|project| project.id.as_str()),
        now,
    )?;
    let mut ranked = Vec::new();
    for item in &items {
        let (open_pr, branch_activity) = signals.for_ref(&item.reference);
        let mut inputs = RankingInputs::at(now);
        inputs.blocking_fan_out = fan_out.get(&item.id).copied().unwrap_or(0);
        inputs.initiative_weight = item
            .initiative_id
            .as_ref()
            .and_then(|id| weights.get(id).copied())
            .unwrap_or(0);
        inputs.is_terminal = crate::core::TERMINAL_STATES.contains(&item.state.as_str());
        inputs.open_pr = open_pr;
        inputs.branch_activity_seconds = branch_activity;
        let score = score_item(&Rankable::from_work_item(item), &inputs);
        ranked.push(RankedRow {
            score: score.total,
            reference: item.reference.clone(),
            value: ranked_row("declared", true, item, &score.total),
        });
    }
    let declared = ranked.len() as i64;

    let (observed, suppressed, closed) = if ctx.observed.has_evidence_tables()? {
        let mut kinds = vec!["forge.issue".to_string(), "marker".to_string()];
        if query.include_prs {
            kinds.push("forge.pull_request".to_string());
        }
        let observations = ctx.observed.latest(
            &kinds,
            project.as_ref().map(|project| project.id.as_str()),
            false,
            true,
            1000,
        )?;
        let keys: Vec<String> = observations
            .iter()
            .map(|observation| observation.subject_key.clone())
            .collect();
        let adopted = view.work_links_for_subjects(&keys)?;
        let overlays = view.work_overlays(&keys)?;
        let confirmed = ctx.observed.last_confirmed(&keys)?;
        let filter_set = SuppressionFilter::build(&view.list_suppressions(false, 1000)?);
        let mut covered: std::collections::HashSet<String> =
            ranked.iter().map(|row| row.reference.clone()).collect();
        let mut suppressed = 0;
        let mut observed_count = 0;
        let mut closed_by_overlay = 0;
        for observation in &observations {
            if !crate::decisions::is_worklike(observation) {
                continue;
            }
            if filter_set.hides(&observation.subject_key, observation.project_id.as_deref()) {
                suppressed += 1;
                continue;
            }
            let targets = crate::decisions::implemented_targets(observation);
            if !targets.is_empty() && targets.iter().any(|target| covered.contains(target)) {
                continue;
            }
            if adopted.contains_key(&observation.subject_key) {
                continue;
            }
            // The upstream-truth join: on a linked project a forge issue is the
            // work item itself, so its overlay refines priority and state. A
            // vogt-only terminal state drops the entry the way an upstream
            // closure would.
            let overlay = overlays.get(&observation.subject_key);
            let mut state = "observed".to_string();
            let mut priority = crate::decisions::priority_of(observation).to_string();
            if observation.kind == "forge.issue" {
                if let Some(project_id) = observation.project_id.as_deref() {
                    if view
                        .project_by_id(project_id)?
                        .is_some_and(|project| crate::application::upstream::is_linked(&project))
                    {
                        let workflow =
                            view.workflow_for(crate::decisions::work_kind_of(observation))?;
                        state = crate::decisions::upstream_state(
                            observation,
                            overlay,
                            &workflow.initial_state,
                        );
                        if crate::core::TERMINAL_STATES.contains(&state.as_str()) {
                            closed_by_overlay += 1;
                            continue;
                        }
                        if let Some(overlay_priority) = overlay.and_then(|overlay| overlay.priority)
                        {
                            priority = overlay_priority.to_string();
                        }
                    }
                }
            }
            let kind = crate::decisions::work_kind_of(observation);
            if !query.kinds.is_empty() && !query.kinds.iter().any(|wanted| wanted == kind) {
                continue;
            }
            if !query.priorities.is_empty()
                && !query.priorities.iter().any(|wanted| wanted == &priority)
            {
                continue;
            }
            let trust = trust_for(
                ctx,
                observation.observed_at,
                confirmed.get(&observation.subject_key).copied(),
            );
            if !query.trust_states.is_empty()
                && !query.trust_states.iter().any(|wanted| wanted == &trust)
            {
                continue;
            }
            let (open_pr, branch_activity) = signals.for_ref(&observation.subject_key);
            let mut inputs = RankingInputs::at(now);
            inputs.open_pr = open_pr;
            inputs.branch_activity_seconds = branch_activity;
            let rankable = Rankable {
                id: observation.subject_key.clone(),
                reference: observation.subject_key.clone(),
                priority: priority.clone(),
                updated_at: observation.observed_at,
                trust_state: trust.clone(),
                state: "observed".to_string(),
                has_initiative: false,
            };
            let score = score_item(&rankable, &inputs);
            let project_slug = observation
                .project_id
                .as_ref()
                .and_then(|id| view.project_by_id(id).ok().flatten())
                .map(|project| project.slug);
            ranked.push(RankedRow {
                score: score.total,
                reference: observation.subject_key.clone(),
                value: observed_row(
                    observation,
                    &ObservedFields {
                        kind,
                        priority: &priority,
                        trust: &trust,
                        state: &state,
                        project_slug: project_slug.as_deref(),
                        adopted_as: adopted.get(&observation.subject_key).map(String::as_str),
                    },
                    &score.total,
                ),
            });
            covered.insert(observation.subject_key.clone());
            observed_count += 1;
        }
        let closed = ctx
            .observed
            .count_closed(&kinds, project.as_ref().map(|project| project.id.as_str()))?
            + closed_by_overlay;
        (observed_count, suppressed, closed)
    } else {
        (0, 0, 0)
    };

    ranked.sort_by(|left, right| {
        right
            .score
            .partial_cmp(&left.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(left.reference.cmp(&right.reference))
    });
    Ok(Gathered {
        ranked,
        declared,
        observed,
        suppressed,
        closed,
        excluded_unlinked,
    })
}

fn initiative_weights(
    view: &impl ReadView,
) -> Result<std::collections::HashMap<String, i64>, VogtError> {
    Ok(view
        .list_initiatives(10_000, 0)?
        .into_iter()
        .map(|initiative| (initiative.id, initiative.weight))
        .collect())
}

/// A declared row as the wire's `RankedItem`.
fn ranked_row(
    origin: &str,
    classified: bool,
    item: &crate::core::WorkItem,
    score: &f64,
) -> serde_json::Value {
    serde_json::json!({
        "origin": origin,
        "classified": classified,
        "ref": item.reference,
        "title": item.title,
        "kind": item.kind.to_string(),
        "state": item.state,
        "priority": item.priority.to_string(),
        "project_slug": item.project_slug,
        "trust_state": item.trust_state.to_string(),
        "labels": item.labels,
        "score": score,
        "updated_at": item.updated_at,
        "item": item,
    })
}

/// An observed subject as the wire's `RankedItem`.
/// The observed fields a ranked row shows, gathered so the row builder takes
/// one argument rather than one per field.
struct ObservedFields<'a> {
    kind: &'a str,
    priority: &'a str,
    trust: &'a str,
    state: &'a str,
    project_slug: Option<&'a str>,
    adopted_as: Option<&'a str>,
}

fn observed_row(
    observation: &crate::core::Observation,
    fields: &ObservedFields<'_>,
    score: &f64,
) -> serde_json::Value {
    serde_json::json!({
        "origin": "observed",
        "classified": crate::decisions::is_classified(observation),
        "ref": observation.subject_key,
        "title": crate::decisions::title_of(observation),
        "kind": fields.kind,
        "state": fields.state,
        "priority": fields.priority,
        "project_slug": fields.project_slug,
        "trust_state": fields.trust,
        "labels": observation.payload.get("labels").cloned().unwrap_or(serde_json::json!([])),
        "score": score,
        "updated_at": observation.observed_at,
        "observation_kind": observation.kind,
        "source_url": observation.source_url,
        "observed_at": observation.observed_at,
        "adopted_as": fields.adopted_as,
    })
}
