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

pub(crate) fn why<C: Clock, I: IdFactory>(
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
///
/// Confirmation is last-seen, not last-changed: the more recent of first-seen
/// and last-confirmed is what the horizon is measured from. A confirmation
/// older than first-seen does not pull the subject backwards. Ports
/// `_freshest` in `services/views.py`.
pub fn trust_for<C: Clock, I: IdFactory>(
    ctx: &AppContext<C, I>,
    observed_at: Moment,
    confirmed_at: Option<Moment>,
) -> String {
    let reference = match confirmed_at {
        Some(confirmed) if confirmed.unix_seconds() > observed_at.unix_seconds() => confirmed,
        _ => observed_at,
    };
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
                && crate::glob::glob_match(pattern, subject_key)
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
    /// Candidates that survive every filter, scored or not: `ranked.len()`
    /// when scoring, the same count when it was skipped (WI-700).
    considered: i64,
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

/// The two candidate counts a Board scope reports, as Python's
/// `candidate_population` gives them: the observed-inclusive count of ranked
/// candidates, and the declared-only count. It goes through `gather`, so it
/// reads the clock the way the Backlog does.
pub(crate) fn candidate_population<C: Clock, I: IdFactory>(
    ctx: &AppContext<C, I>,
    project: Option<&str>,
    kinds: Vec<String>,
    priorities: Vec<String>,
    assignee: Option<&str>,
    initiative: Option<&str>,
    label: Option<&str>,
) -> Result<(i64, i64), VogtError> {
    let query = GatherQuery {
        project,
        kinds,
        priorities,
        assignee,
        initiative,
        label,
        trust_states: Vec::new(),
        include_prs: true,
    };
    let gathered = gather(ctx, &query)?;
    Ok((gathered.ranked.len() as i64, gathered.declared))
}

/// What `project.brief` reads from the backlog gather for one project: the
/// open-work and open-bug counts, the declared and observed split, and the top
/// rows. Python's `brief_project` calls `_gather` with no filters and then
/// takes these from it, so this is the same gather `backlog` ranks.
pub(crate) struct BriefPopulation {
    pub open_work: i64,
    pub open_bugs: i64,
    pub declared: i64,
    pub observed: i64,
    pub top: Vec<serde_json::Value>,
}

pub(crate) fn brief_population<C: Clock, I: IdFactory>(
    ctx: &AppContext<C, I>,
    project: &str,
    backlog_limit: usize,
    mode: &str,
) -> Result<BriefPopulation, VogtError> {
    let query = GatherQuery {
        project: Some(project),
        kinds: Vec::new(),
        priorities: Vec::new(),
        assignee: None,
        initiative: None,
        label: None,
        trust_states: Vec::new(),
        include_prs: true,
    };
    let gathered = gather(ctx, &query)?;
    let open_bugs = gathered
        .ranked
        .iter()
        .filter(|row| row.value.get("kind").and_then(serde_json::Value::as_str) == Some("bug"))
        .count() as i64;
    Ok(BriefPopulation {
        open_work: gathered.ranked.len() as i64,
        open_bugs,
        declared: gathered.declared,
        observed: gathered.observed,
        top: gathered
            .ranked
            .iter()
            .take(backlog_limit)
            .map(|row| projected(&row.value, mode))
            .collect(),
    })
}

/// `backlog(limit=1).total_considered`, the backlog badge `place.metrics`
/// reads. It is the whole candidate population, not the page.
pub(crate) fn backlog_total_considered<C: Clock, I: IdFactory>(
    ctx: &AppContext<C, I>,
) -> Result<i64, VogtError> {
    // Counted in the stores (WI-4448): the declared rows with a COUNT, the
    // observed candidates with one query that applies the ranked backlog's
    // drops. No row is loaded and no trust is computed per row, and the count
    // equals the ranked list's length. `backlog` reads the freshness clock
    // after the gather, and so does this.
    let query = GatherQuery {
        project: None,
        kinds: Vec::new(),
        priorities: Vec::new(),
        assignee: None,
        initiative: None,
        label: None,
        trust_states: Vec::new(),
        include_prs: true,
    };
    let view = ctx.declared.read()?;
    let filter = declared_filter(&view, None, &query)?;
    let declared = view.count_work_items(&filter)?.min(filter.limit);
    let observed = if ctx.observed.has_evidence_tables()? {
        let inputs = crate::storage::interface::BacklogCountInputs {
            bug_initial_state: view.workflow_for("bug")?.initial_state,
            feature_initial_state: view.workflow_for("feature")?.initial_state,
        };
        ctx.observed
            .count_backlog_candidates(&ctx.config.declared_db_path(), &inputs)?
    } else {
        0
    };
    // The stepping clock still advances as the ranked gather would advance it
    // (WI-4462): the git-signals read (`views.py:423`), one `trust_for` read
    // per observed candidate (`views.py:189`; no kind or priority filter here,
    // so every candidate is counted), and the `_score_all` read
    // (`views.py:554`). Skipping them put every later fake-clock stamp behind
    // Python's.
    for _ in 0..observed + 2 {
        let _ = now_of(&ctx.clock);
    }
    crate::application::services::freshness::freshness_of(&ctx.observed, &ctx.clock)?;
    Ok(declared + observed)
}

/// How many declared rows a ranked view loads before scoring. Python's
/// `RANKING_CANDIDATE_LIMIT` (`views.py`). The store default is 100, which
/// silently drops the rest of a large backlog.
const RANKING_CANDIDATE_LIMIT: i64 = 1000;

/// The candidate population of a ranked view, scored and ordered. Ports
/// `_gather`: declared rows first, then the observed subjects that name no
/// declared work, with a PR collapsed under the item it implements.
fn gather<C: Clock, I: IdFactory>(
    ctx: &AppContext<C, I>,
    query: &GatherQuery<'_>,
) -> Result<Gathered, VogtError> {
    gather_with(ctx, query, true)
}

/// The observed kinds a ranked view reads. Pull requests join when the query
/// wants them.
fn observed_kinds(include_prs: bool) -> Vec<String> {
    let mut kinds = vec!["forge.issue".to_string(), "marker".to_string()];
    if include_prs {
        kinds.push("forge.pull_request".to_string());
    }
    kinds
}

/// The kind and priority filters a ranked view applies after the drops. Both
/// the ranked gather and the count apply them, so they count the same rows.
fn wanted_by(query: &GatherQuery<'_>, kind: &str, priority: &str) -> bool {
    (query.kinds.is_empty() || query.kinds.iter().any(|wanted| wanted == kind))
        && (query.priorities.is_empty() || query.priorities.iter().any(|wanted| wanted == priority))
}

/// The store filter for the declared half of a ranked view. Kinds and
/// priorities filter the declared half in the store, as Python's `WorkFilter`
/// does (`views.py:393-414`).
fn declared_filter(
    view: &impl ReadView,
    project_id: Option<String>,
    query: &GatherQuery<'_>,
) -> Result<crate::storage::interface::WorkFilter, VogtError> {
    let exclude_unlinked_native = project_id.is_none();
    Ok(crate::storage::interface::WorkFilter {
        project_id,
        kinds: query.kinds.clone(),
        priorities: query.priorities.clone(),
        assignee_actor_id: query
            .assignee
            .map(|identity| {
                crate::application::resolve::actor(view, identity).map(|actor| actor.id)
            })
            .transpose()?,
        initiative_id: query
            .initiative
            .map(|slug| {
                crate::application::resolve::initiative(view, slug).map(|initiative| initiative.id)
            })
            .transpose()?,
        label: query.label.map(str::to_string),
        trust_states: query.trust_states.clone(),
        exclude_terminal: true,
        exclude_unlinked_native,
        limit: RANKING_CANDIDATE_LIMIT,
        ..crate::storage::interface::WorkFilter::default()
    })
}

/// What the observed half of a ranked view reads before any drop: the latest
/// observations and what they are linked to. Shared by the ranked gather and
/// the count, so both drop the same subjects.
struct ObservedInputs {
    observations: Vec<crate::core::Observation>,
    adopted: std::collections::BTreeMap<String, String>,
    overlays: std::collections::BTreeMap<String, crate::core::WorkOverlay>,
    projects: std::collections::HashMap<String, crate::core::Project>,
    filter_set: SuppressionFilter,
}

fn observed_inputs<C: Clock, I: IdFactory>(
    ctx: &AppContext<C, I>,
    view: &impl ReadView,
    project_id: Option<&str>,
    include_prs: bool,
) -> Result<ObservedInputs, VogtError> {
    let observations =
        ctx.observed
            .latest(&observed_kinds(include_prs), project_id, false, true, 1000)?;
    let keys: Vec<String> = observations
        .iter()
        .map(|observation| observation.subject_key.clone())
        .collect();
    let adopted = view.work_links_for_subjects(&keys)?;
    // Projects are loaded once, as Python's `views.py` does (`projects = {p.id: p ...}`).
    // `project_by_id` per observed row was a full table lookup for every candidate.
    let projects: std::collections::HashMap<String, crate::core::Project> = view
        .list_projects(10_000, 0)?
        .into_iter()
        .map(|project| (project.id.clone(), project))
        .collect();
    // An overlay only refines a forge issue on a linked project (the upstream
    // truth join), so only those subjects' overlays are read.
    let overlay_keys: Vec<String> = observations
        .iter()
        .filter(|observation| {
            observation.kind == "forge.issue"
                && observation
                    .project_id
                    .as_deref()
                    .and_then(|id| projects.get(id))
                    .is_some_and(crate::application::upstream::is_linked)
        })
        .map(|observation| observation.subject_key.clone())
        .collect();
    let overlays = view.work_overlays(&overlay_keys)?;
    let filter_set = SuppressionFilter::build(&view.list_suppressions(false, 1000)?);
    Ok(ObservedInputs {
        observations,
        adopted,
        overlays,
        projects,
        filter_set,
    })
}

/// An observed subject that survives every drop, with the fields a row
/// renders. Trust and the git signals are added by the ranked gather only.
struct ObservedCandidate<'a> {
    observation: &'a crate::core::Observation,
    kind: &'static str,
    priority: String,
    state: String,
    project_slug: Option<String>,
    adopted_as: Option<String>,
}

struct ObservedCandidates<'a> {
    rows: Vec<ObservedCandidate<'a>>,
    suppressed: i64,
    closed_by_overlay: i64,
}

/// Every drop a ranked view makes on the observed half, in Python's order.
/// Reads no clock: the per-row trust read belongs to the ranked gather.
fn observed_candidates<'a>(
    inputs: &'a ObservedInputs,
    view: &impl ReadView,
) -> Result<ObservedCandidates<'a>, VogtError> {
    // Every observed subject is present before any PR is dropped, so a PR
    // collapses under the issue it implements even when that issue is seen
    // later. Adopted subjects count too; declared refs do not — a PR
    // implementing a WI-n is its own row. Python's `present_subjects`.
    let mut present: std::collections::HashSet<String> = inputs
        .observations
        .iter()
        .map(|observation| observation.subject_key.clone())
        .collect();
    present.extend(inputs.adopted.keys().cloned());
    let mut rows = Vec::new();
    let mut suppressed = 0;
    let mut closed_by_overlay = 0;
    for observation in &inputs.observations {
        if !crate::decisions::is_worklike(observation) {
            continue;
        }
        if inputs
            .filter_set
            .hides(&observation.subject_key, observation.project_id.as_deref())
        {
            suppressed += 1;
            continue;
        }
        let targets = crate::decisions::implemented_targets(observation);
        if !targets.is_empty() && targets.iter().any(|target| present.contains(target)) {
            continue;
        }
        if inputs.adopted.contains_key(&observation.subject_key) {
            continue;
        }
        // The upstream-truth join: on a linked project a forge issue is the
        // work item itself, so its overlay refines priority and state. A
        // vogt-only terminal state drops the entry the way an upstream
        // closure would.
        let overlay = inputs.overlays.get(&observation.subject_key);
        let mut state = "observed".to_string();
        let mut priority = crate::decisions::priority_of(observation).to_string();
        if observation.kind == "forge.issue" {
            if let Some(project_id) = observation.project_id.as_deref() {
                if inputs
                    .projects
                    .get(project_id)
                    .is_some_and(crate::application::upstream::is_linked)
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
                    if let Some(overlay_priority) = overlay.and_then(|overlay| overlay.priority) {
                        priority = overlay_priority.to_string();
                    }
                }
            }
        }
        let project_slug = observation
            .project_id
            .as_ref()
            .and_then(|id| inputs.projects.get(id))
            .map(|project| project.slug.clone());
        rows.push(ObservedCandidate {
            observation,
            kind: crate::decisions::work_kind_of(observation),
            priority,
            state,
            project_slug,
            adopted_as: inputs.adopted.get(&observation.subject_key).cloned(),
        });
    }
    Ok(ObservedCandidates {
        rows,
        suppressed,
        closed_by_overlay,
    })
}

/// [`gather`], with `score` off when only the candidates are wanted. The clock
/// reads that scoring needs are skipped with it, so the ranked order is only
/// built when `score` is on (WI-700, WI-4448).
fn gather_with<C: Clock, I: IdFactory>(
    ctx: &AppContext<C, I>,
    query: &GatherQuery<'_>,
    score: bool,
) -> Result<Gathered, VogtError> {
    let view = ctx.declared.read()?;
    let project = query
        .project
        .map(|slug| crate::application::resolve::project(&view, slug))
        .transpose()?;
    let project_id = project.as_ref().map(|project| project.id.clone());
    let filter = declared_filter(&view, project_id.clone(), query)?;
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
    let signals = git_signals(ctx, project_id.as_deref(), now_of(&ctx.clock))?;
    // Scoring waits until both halves are assembled. The candidates carry their
    // ranking inputs; the clock they score against is read once, afterwards.
    let mut declared_rows: Vec<DeclaredScore> = Vec::new();
    for item in &items {
        let (open_pr, branch_activity) = signals.for_ref(&item.reference);
        declared_rows.push(DeclaredScore {
            item,
            open_pr,
            branch_activity,
            fan_out: fan_out.get(&item.id).copied().unwrap_or(0),
            initiative_weight: item
                .initiative_id
                .as_ref()
                .and_then(|id| weights.get(id).copied())
                .unwrap_or(0),
        });
    }
    let declared = declared_rows.len() as i64;

    let (mut ranked, considered, observed, suppressed, closed) =
        if ctx.observed.has_evidence_tables()? {
            let inputs = observed_inputs(ctx, &view, project_id.as_deref(), query.include_prs)?;
            let keys: Vec<String> = inputs
                .observations
                .iter()
                .map(|observation| observation.subject_key.clone())
                .collect();
            let confirmed = ctx.observed.last_confirmed(&keys)?;
            let candidates = observed_candidates(&inputs, &view)?;
            // Python reads the clock once per observed candidate inside `trust_for`
            // (`views.py:189`), for every candidate before the kind and priority
            // filters. The filters come after, so the reads stay the same.
            let observed_rows: Vec<ObservedScore<'_>> = candidates
                .rows
                .into_iter()
                .map(|row| {
                    let trust = trust_for(
                        ctx,
                        row.observation.observed_at,
                        confirmed.get(&row.observation.subject_key).copied(),
                    );
                    let (open_pr, branch_activity) = signals.for_ref(&row.observation.subject_key);
                    ObservedScore {
                        observation: row.observation,
                        kind: row.kind,
                        priority: row.priority,
                        trust,
                        state: row.state,
                        project_slug: row.project_slug,
                        adopted_as: row.adopted_as,
                        open_pr,
                        branch_activity,
                    }
                })
                .collect();
            let observed_count = observed_rows.len() as i64;
            let observed_rows: Vec<ObservedScore<'_>> = observed_rows
                .into_iter()
                .filter(|row| wanted_by(query, row.kind, &row.priority))
                .collect();
            let closed = ctx
                .observed
                .count_closed(&observed_kinds(query.include_prs), project_id.as_deref())?
                + candidates.closed_by_overlay;
            let considered = (declared_rows.len() + observed_rows.len()) as i64;
            // Python reads the clock at `views.py:423` for git signals, once per
            // observed row inside `trust_for` (`views.py:189`) while assembling
            // them, then once at `views.py:554` for `_score_all`. Both halves
            // score from that last read, so it comes after the observed loop.
            let scored = if score {
                score_rows(&declared_rows, &observed_rows, now_of(&ctx.clock))
            } else {
                let _ = now_of(&ctx.clock);
                Vec::new()
            };
            (
                scored,
                considered,
                observed_count,
                candidates.suppressed,
                closed,
            )
        } else {
            // No observed rows, so no `trust_for` reads: the scoring read is the
            // second one, straight after the git-signals read. The declared rows
            // were already filtered by kind and priority in the store query.
            let considered = declared_rows.len() as i64;
            let scored = if score {
                score_rows(&declared_rows, &[], now_of(&ctx.clock))
            } else {
                let _ = now_of(&ctx.clock);
                Vec::new()
            };
            (scored, considered, 0, 0, 0)
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
        considered,
        declared,
        observed,
        suppressed,
        closed,
        excluded_unlinked,
    })
}

fn score_rows(
    declared_rows: &[DeclaredScore<'_>],
    observed_rows: &[ObservedScore<'_>],
    now: Moment,
) -> Vec<RankedRow> {
    let mut scored = Vec::new();
    for row in declared_rows {
        let mut inputs = RankingInputs::at(now);
        inputs.blocking_fan_out = row.fan_out;
        inputs.initiative_weight = row.initiative_weight;
        inputs.is_terminal = crate::core::TERMINAL_STATES.contains(&row.item.state.as_str());
        inputs.open_pr = row.open_pr;
        inputs.branch_activity_seconds = row.branch_activity;
        let score = score_item(&Rankable::from_work_item(row.item), &inputs);
        scored.push(RankedRow {
            score: score.total,
            reference: row.item.reference.clone(),
            value: ranked_row("declared", true, row.item, &score.total),
        });
    }
    for row in observed_rows {
        let mut inputs = RankingInputs::at(now);
        inputs.open_pr = row.open_pr;
        inputs.branch_activity_seconds = row.branch_activity;
        let rankable = Rankable {
            id: row.observation.subject_key.clone(),
            reference: row.observation.subject_key.clone(),
            priority: row.priority.clone(),
            updated_at: row.observation.observed_at,
            trust_state: row.trust.clone(),
            state: "observed".to_string(),
            has_initiative: false,
        };
        let score = score_item(&rankable, &inputs);
        scored.push(RankedRow {
            score: score.total,
            reference: row.observation.subject_key.clone(),
            value: observed_row(
                row.observation,
                &ObservedFields {
                    kind: row.kind,
                    priority: &row.priority,
                    trust: &row.trust,
                    state: &row.state,
                    project_slug: row.project_slug.as_deref(),
                    adopted_as: row.adopted_as.as_deref(),
                },
                &score.total,
            ),
        });
    }
    scored
}

struct DeclaredScore<'a> {
    item: &'a crate::core::WorkItem,
    open_pr: bool,
    branch_activity: Option<i64>,
    fan_out: i64,
    initiative_weight: i64,
}

struct ObservedScore<'a> {
    observation: &'a crate::core::Observation,
    kind: &'static str,
    priority: String,
    trust: String,
    state: String,
    project_slug: Option<String>,
    adopted_as: Option<String>,
    open_pr: bool,
    branch_activity: Option<i64>,
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
        crate::application::services::freshness::freshness_of(&ctx.observed, &ctx.clock)?;
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

/// `bugs`, as the registry calls it.
pub fn bugs_op(ctx: &Built, params: serde_json::Value) -> Result<serde_json::Value, VogtError> {
    crate::with_ctx!(ctx, |ctx| bugs(ctx, params))
}

/// Open bugs across every project. Ports `bugs`.
///
/// The backlog's gather narrowed to kind `bug`, with no initiative or trust
/// filter. The rows come back whole, and `next_offset` stays null: Python
/// slices the ranked list and never sets the page cursor.
fn bugs<C: Clock, I: IdFactory>(
    ctx: &AppContext<C, I>,
    params: serde_json::Value,
) -> Result<serde_json::Value, VogtError> {
    let project = params.get("project").and_then(serde_json::Value::as_str);
    let limit = params
        .get("limit")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(50);
    let offset = params
        .get("offset")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(0);
    if let Some(project) = project {
        if let Some(marker) = unlinked_scope(ctx, project)? {
            return Ok(marker);
        }
    }
    let query = GatherQuery {
        project,
        kinds: vec!["bug".to_string()],
        priorities: string_list(params.get("priorities")),
        assignee: params.get("assignee").and_then(serde_json::Value::as_str),
        initiative: None,
        label: params.get("label").and_then(serde_json::Value::as_str),
        trust_states: Vec::new(),
        include_prs: true,
    };
    let gathered = gather(ctx, &query)?;
    let freshness =
        crate::application::services::freshness::freshness_of(&ctx.observed, &ctx.clock)?;
    let total = gathered.ranked.len() as i64;
    let start = offset.max(0) as usize;
    let page: Vec<serde_json::Value> = gathered
        .ranked
        .iter()
        .skip(start)
        .take(limit.max(0) as usize)
        .map(|row| row.value.clone())
        .collect();
    Ok(serde_json::json!({
        "items": page,
        "total_considered": total,
        "next_offset": serde_json::Value::Null,
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
        crate::application::services::freshness::freshness_of(&ctx.observed, &ctx.clock)?;
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
    use super::{
        backlog, backlog_total_considered, now_of, projected, trust_for, RANKING_CANDIDATE_LIMIT,
    };
    use crate::application::context::{build_context, Built};
    use crate::config::VogtConfig;
    use crate::core::{
        ActorKind, LinkState, Moment, Origin, Principal, Priority, Project, SequentialIds,
        StepClock, TrustState, WorkItem, WorkKind,
    };
    use crate::storage::interface::{DeclaredStore, WriteTxn};

    fn unique() -> u64 {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        COUNTER.fetch_add(1, Ordering::Relaxed)
    }

    /// `trust_for` reads the horizon and the clock. The stores are never opened.
    fn trust_context(now: Moment) -> Built {
        let dir =
            std::env::temp_dir().join(format!("vogt-trust-{}-{}", std::process::id(), unique()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        build_context(
            VogtConfig {
                data_dir: dir,
                verify_horizon_hours: 24,
                ..VogtConfig::default()
            },
            Some(Principal::new("local:trust", ActorKind::Human, "Trust").unwrap()),
            Some(StepClock::new(now)),
            Some(SequentialIds::new(None).unwrap()),
            None,
            None,
            None,
            None,
        )
        .unwrap()
    }

    fn with_trust(
        now: Moment,
        body: impl FnOnce(&crate::application::context::AppContext<StepClock, SequentialIds>),
    ) {
        match trust_context(now) {
            Built::StepSequential(ctx) => body(&ctx),
            _other => panic!("expected a step clock and sequential ids"),
        }
    }

    #[test]
    fn a_reconfirmed_subject_stays_verified() {
        // Ports `tests/test_trust_confirmation.py`. First-seen long ago is
        // stale on its own; a confirmation five minutes ago keeps it verified.
        let now = Moment::from_unix(1_781_000_000, 0);
        let horizon = 24 * 3600;
        let stale_observed = Moment::from_unix(now.unix_seconds() - horizon - 3600, 0);
        let fresh_confirmed = Moment::from_unix(now.unix_seconds() - 5 * 60, 0);
        with_trust(now, |ctx| {
            assert_eq!(trust_for(ctx, stale_observed, None), "stale");
            assert_eq!(
                trust_for(ctx, stale_observed, Some(fresh_confirmed)),
                "verified"
            );
        });
    }

    #[test]
    fn confirmation_does_not_rescue_a_genuinely_stale_subject() {
        // Confirmed, but the confirmation itself aged out. That is still stale.
        let now = Moment::from_unix(1_781_000_000, 0);
        let horizon = 24 * 3600;
        let old = Moment::from_unix(now.unix_seconds() - horizon - 2 * 3600, 0);
        with_trust(now, |ctx| {
            assert_eq!(trust_for(ctx, old, Some(old)), "stale");
        });
    }

    #[test]
    fn an_older_confirmation_does_not_age_a_fresh_sighting() {
        // The horizon is measured from the more recent of the two times.
        // A confirmation older than first-seen must not pull the subject stale.
        let now = Moment::from_unix(1_781_000_000, 0);
        let horizon = 24 * 3600;
        let fresh = Moment::from_unix(now.unix_seconds() - 60, 0);
        let ancient = Moment::from_unix(now.unix_seconds() - horizon - 3600, 0);
        with_trust(now, |ctx| {
            assert_eq!(trust_for(ctx, fresh, Some(ancient)), "verified");
        });
    }

    #[test]
    fn the_backlog_keeps_declared_items_past_the_store_page() {
        // The store's default work-list limit is 100. A ranked view must load
        // the ranking window (1000), or items after the first page vanish
        // from `declared` and `total_considered`.
        let now = Moment::from_unix(1_781_000_000, 0);
        with_trust(now, |ctx| {
            ctx.declared.migrate().unwrap();
            ctx.declared
                .bootstrap(&Principal::new("local:test-user", ActorKind::Human, "Test").unwrap())
                .unwrap();
            {
                let mut txn = ctx.declared.write().unwrap();
                let mut project = Project::new("prj_alpha", "alpha", "Alpha", "/tmp/alpha", now);
                project.link_state = LinkState::Linked;
                txn.insert_project(&project).unwrap();
                for n in 1..=120 {
                    txn.insert_work_item(&WorkItem {
                        id: format!("wrk_{n}"),
                        reference: format!("WI-{n}"),
                        kind: WorkKind::Feature,
                        title: format!("Item {n}"),
                        body: String::new(),
                        state: "open".to_string(),
                        priority: Priority::P2,
                        effort: None,
                        project_id: Some("prj_alpha".to_string()),
                        project_slug: Some("alpha".to_string()),
                        initiative_id: None,
                        origin: Origin::Created,
                        trust_state: TrustState::Unverified,
                        assignee_actor_id: None,
                        assignee_identity_ref: None,
                        labels: Vec::new(),
                        relations: Vec::new(),
                        superseded_by: None,
                        created_at: now,
                        updated_at: now,
                    })
                    .unwrap();
                }
                txn.commit().unwrap();
            }
            let result = backlog(ctx, serde_json::json!({"limit": 20})).unwrap();
            assert_eq!(result["declared"], 120);
            assert_eq!(result["total_considered"], 120);
            assert_eq!(result["items"].as_array().unwrap().len(), 20);
        });
    }

    /// A ranked view loads at most `RANKING_CANDIDATE_LIMIT` declared rows.
    /// Ports `test_the_backlog_is_capped_rather_than_unbounded`: seeding past
    /// the window must not make `total_considered` grow without bound.
    #[test]
    fn the_backlog_is_capped_rather_than_unbounded() {
        let now = Moment::from_unix(1_781_000_000, 0);
        with_trust(now, |ctx| {
            ctx.declared.migrate().unwrap();
            ctx.declared
                .bootstrap(&Principal::new("local:test-user", ActorKind::Human, "Test").unwrap())
                .unwrap();
            let seeded = RANKING_CANDIDATE_LIMIT + 25;
            {
                let mut txn = ctx.declared.write().unwrap();
                let mut project = Project::new("prj_alpha", "alpha", "Alpha", "/tmp/alpha", now);
                project.link_state = LinkState::Linked;
                txn.insert_project(&project).unwrap();
                for n in 1..=seeded {
                    txn.insert_work_item(&WorkItem {
                        id: format!("wrk_{n}"),
                        reference: format!("WI-{n}"),
                        kind: WorkKind::Feature,
                        title: format!("Item {n}"),
                        body: String::new(),
                        state: "open".to_string(),
                        priority: Priority::P2,
                        effort: None,
                        project_id: Some("prj_alpha".to_string()),
                        project_slug: Some("alpha".to_string()),
                        initiative_id: None,
                        origin: Origin::Created,
                        trust_state: TrustState::Unverified,
                        assignee_actor_id: None,
                        assignee_identity_ref: None,
                        labels: Vec::new(),
                        relations: Vec::new(),
                        superseded_by: None,
                        created_at: now,
                        updated_at: now,
                    })
                    .unwrap();
                }
                txn.commit().unwrap();
            }
            let result = backlog(ctx, serde_json::json!({"limit": 1})).unwrap();
            let considered = result["total_considered"].as_i64().unwrap();
            assert!(
                considered <= RANKING_CANDIDATE_LIMIT,
                "total_considered {considered} exceeded the ranking window {RANKING_CANDIDATE_LIMIT}"
            );
            assert_eq!(considered, RANKING_CANDIDATE_LIMIT);
        });
    }

    #[test]
    fn the_backlog_count_steps_the_clock_like_the_ranked_backlog() {
        // `place.metrics` counts the backlog without ranking it (WI-4448), but
        // the stepping clock must land where `backlog(limit=1)` leaves it, or
        // every later fake-clock stamp drifts from Python's (WI-4462).
        // Two observed issues: Python reads the clock once per observed
        // candidate inside `trust_for`, so the count must step for each.
        use crate::core::SweepOutcome;
        use crate::storage::interface::ObservedStore;
        use crate::storage::observed_types::PendingObservation;
        let now = Moment::from_unix(1_781_000_000, 0);
        with_trust(now, |ctx| {
            ctx.declared.migrate().unwrap();
            ctx.observed.migrate().unwrap();
            ctx.declared
                .bootstrap(&Principal::new("local:test-user", ActorKind::Human, "Test").unwrap())
                .unwrap();
            let sweep = ctx
                .observed
                .begin_sweep("test-forge", &["vogt".to_string()], now)
                .unwrap();
            let findings: Vec<PendingObservation> = (0..2)
                .map(|n| PendingObservation {
                    kind: "forge.issue".into(),
                    subject_key: format!("vogt/issues/{n}"),
                    payload: serde_json::json!({"state": "open", "title": format!("issue {n}"), "number": n}),
                    content_digest: format!("issue-{n}"),
                    project_id: None,
                    source_url: None,
                    promoted: false,
                })
                .collect();
            ctx.observed.append(&sweep.id, &findings, now).unwrap();
            ctx.observed
                .finish_sweep(&sweep.id, SweepOutcome::Ok, &[], now, None)
                .unwrap();
            ctx.observed.rebuild_latest().unwrap();
            assert_eq!(backlog_total_considered(ctx).unwrap(), 2);
            let steps = |run: &dyn Fn()| {
                let before = now_of(&ctx.clock).unix_seconds();
                run();
                now_of(&ctx.clock).unix_seconds() - before
            };
            let ranked = steps(&|| {
                backlog(ctx, serde_json::json!({"limit": 1})).unwrap();
            });
            let counted = steps(&|| {
                backlog_total_considered(ctx).unwrap();
            });
            assert_eq!(counted, ranked);
        });
    }

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
