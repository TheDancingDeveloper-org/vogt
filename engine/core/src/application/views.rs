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
        now_of(&ctx.clock),
    )?;
    // The one clock read the scoring uses, taken after the candidate reads, as
    // Python's `_score_all(..., now=ctx.clock())` does.
    let now = now_of(&ctx.clock);
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
        // Every observed subject is present before any PR is dropped, so a PR
        // collapses under the issue it implements even when that issue is seen
        // later. Adopted subjects count too; declared refs do not — a PR
        // implementing a WI-n is its own row. Python's `present_subjects`.
        let mut present: std::collections::HashSet<String> = observations
            .iter()
            .map(|observation| observation.subject_key.clone())
            .collect();
        present.extend(adopted.keys().cloned());
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
            if !targets.is_empty() && targets.iter().any(|target| present.contains(target)) {
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
            // Counted before the kind and priority filters, as Python's
            // `observed.append` is: a filtered-out subject still counts.
            observed_count += 1;
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
            // Trust narrows only the declared half. Python passes `trust_states`
            // into the work filter and never checks it against an observed
            // subject, so a stale observed row survives `--trust-states verified`.
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
        "observation_kind": serde_json::Value::Null,
        "source_url": serde_json::Value::Null,
        "observed_at": serde_json::Value::Null,
        "adopted_as": serde_json::Value::Null,
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
        "item": serde_json::Value::Null,
        "observation_kind": observation.kind,
        "source_url": observation.source_url,
        "observed_at": observation.observed_at,
        "adopted_as": fields.adopted_as,
    })
}

/// `backlog`, as the registry calls it.
pub fn backlog_op(ctx: &Built, params: serde_json::Value) -> Result<serde_json::Value, VogtError> {
    crate::with_ctx!(ctx, |ctx| backlog(ctx, params))
}

/// The ranked backlog, globally or for one project. Ports `backlog`.
///
/// Ranking is computed over the whole candidate set before the slice, so page
/// two is the next rows of one ordering rather than a fresh ranking of what was
/// left. An unlinked project scope answers with the CTA marker instead of a
/// ranked list.
fn backlog<C: Clock, I: IdFactory>(
    ctx: &AppContext<C, I>,
    params: serde_json::Value,
) -> Result<serde_json::Value, VogtError> {
    let project = params.get("project").and_then(serde_json::Value::as_str);
    let mode = params
        .get("mode")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("summary");
    let limit = params
        .get("limit")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(50);
    let offset = params
        .get("offset")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(0);
    // Freshness is the last clock read, after the unlinked check and the
    // gather, so its age matches Python's (views.py reads it after ranking).
    if let Some(project) = project {
        if let Some(marker) = unlinked_scope(ctx, project)? {
            return Ok(marker);
        }
    }
    let query = GatherQuery {
        project,
        kinds: string_list(params.get("kinds")),
        priorities: string_list(params.get("priorities")),
        assignee: params.get("assignee").and_then(serde_json::Value::as_str),
        initiative: params.get("initiative").and_then(serde_json::Value::as_str),
        label: params.get("label").and_then(serde_json::Value::as_str),
        trust_states: string_list(params.get("trust_states")),
        include_prs: params
            .get("include_prs")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(true),
    };
    let gathered = gather(ctx, &query)?;
    let freshness =
        crate::application::services::freshness::freshness_of(&ctx.observed, now_of(&ctx.clock))?;
    let total = gathered.ranked.len() as i64;
    let start = offset.max(0) as usize;
    let page: Vec<serde_json::Value> = gathered
        .ranked
        .iter()
        .skip(start)
        .take(limit.max(0) as usize)
        .map(|row| projected(&row.value, mode))
        .collect();
    let following = offset + page.len() as i64;
    Ok(serde_json::json!({
        "items": page,
        "total_considered": total,
        "next_offset": (!page.is_empty() && following < total).then_some(following),
        "declared": gathered.declared,
        "observed": gathered.observed,
        "suppressed": gathered.suppressed,
        "closed_upstream": gathered.closed,
        "link_state": project.map(|_| "linked"),
        "excluded_unlinked": gathered.excluded_unlinked,
        "scope": project.unwrap_or("global"),
        "freshness": freshness,
    }))
}

/// The link-or-publish CTA answer for an unlinked project scope, or `None` when
/// the project is linked. `excluded_unlinked` counts the open native items a
/// link or publish would migrate, so the surface can say what the act would
/// carry across.
fn unlinked_scope<C: Clock, I: IdFactory>(
    ctx: &AppContext<C, I>,
    project: &str,
) -> Result<Option<serde_json::Value>, VogtError> {
    let view = ctx.declared.read()?;
    let project_row = crate::application::resolve::project(&view, project)?;
    if crate::application::upstream::is_linked(&project_row) {
        return Ok(None);
    }
    let freshness =
        crate::application::services::freshness::freshness_of(&ctx.observed, now_of(&ctx.clock))?;
    let pending = view.count_work_items(&crate::storage::interface::WorkFilter {
        project_id: Some(project_row.id.clone()),
        exclude_terminal: true,
        ..crate::storage::interface::WorkFilter::default()
    })?;
    Ok(Some(serde_json::json!({
        "items": [],
        "total_considered": 0,
        "next_offset": serde_json::Value::Null,
        "declared": 0,
        "observed": 0,
        "suppressed": 0,
        "closed_upstream": 0,
        "link_state": "unlinked",
        "excluded_unlinked": pending,
        "scope": project,
        "freshness": freshness,
    })))
}

/// `summary` nulls each row's full work item in place, so the key stays where
/// `RankedItem` puts it (after `updated_at`). Inserting it would append the key
/// and every summary row would differ. `full` returns the row untouched.
fn projected(row: &serde_json::Value, mode: &str) -> serde_json::Value {
    if mode == "full" {
        return row.clone();
    }
    let mut summary = row.clone();
    if let Some(item) = summary.get_mut("item") {
        *item = serde_json::Value::Null;
    }
    summary
}

fn string_list(value: Option<&serde_json::Value>) -> Vec<String> {
    value
        .and_then(serde_json::Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::projected;

    #[test]
    fn a_summary_row_nulls_item_in_place() {
        // `RankedItem` puts `item` after `updated_at` and before the observed
        // fields. Nulling it must not move the key to the end.
        let row = serde_json::json!({
            "origin": "declared",
            "classified": true,
            "ref": "WI-1",
            "title": "t",
            "kind": "feature",
            "state": "open",
            "priority": "p2",
            "project_slug": null,
            "trust_state": "unverified",
            "labels": [],
            "score": 1.0,
            "updated_at": "2026-01-01T00:00:00Z",
            "item": {"ref": "WI-1"},
            "observation_kind": null,
            "source_url": null,
            "observed_at": null,
            "adopted_as": null,
        });
        let summary = projected(&row, "summary");
        let keys: Vec<&String> = summary.as_object().unwrap().keys().collect();
        assert_eq!(
            keys,
            [
                "origin",
                "classified",
                "ref",
                "title",
                "kind",
                "state",
                "priority",
                "project_slug",
                "trust_state",
                "labels",
                "score",
                "updated_at",
                "item",
                "observation_kind",
                "source_url",
                "observed_at",
                "adopted_as",
            ]
        );
        assert!(summary["item"].is_null());
        assert_eq!(projected(&row, "full")["item"]["ref"], "WI-1");
    }
}
