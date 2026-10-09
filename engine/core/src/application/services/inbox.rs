//! The normalized attention Inbox and its audited triage actions. Ports
//! `application/services/inbox.py`.
//!
//! The browser receives one server-owned projection. It never merges forge
//! notifications, drift, CI, or live engine state itself; this module does the
//! joins, ordering, coverage disclosure, and occurrence-scoped decisions.
//!
//! The CI re-rollup stays O(checks): one `roll_up` per project, computed from
//! the checks already in hand, and triage is read once through
//! `inbox_triage_by_keys`. Never one query per entry.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use serde_json::{json, Value};

use crate::actors::{self, ActorClass};
use crate::application::context::{AppContext, Built};
use crate::application::services::{context_parts, dispatch, now_of, write_context};
use crate::application::writes::{audited_write, WriteOutcome};
use crate::core::{
    Clock, CodingSession, DriftProposal, IdFactory, InboxTriage, Moment, Observation, Project,
    SessionGrant, Sweep, TriageState, WorkItem,
};
use crate::decisions::{self, digest_of};
use crate::errors::VogtError;
use crate::storage::interface::{DeclaredStore, ObservedStore, ReadView, WriteTxn};
use crate::storage::sqlite::declared::SqliteDeclaredStore;

const MAX_SCAN: i64 = 10_000;
/// Bound overlays considered per read. Python's `ci_watch.MAX_BOUND`.
const MAX_BOUND: i64 = 500;
const KIND_TASK_RUN: &str = "agent_task.run";
const REF_FAILURE_KIND: &str = "ci.ref_failure";
const BRANCH_CONCLUDED_KIND: &str = "ci.branch_concluded";
const DEPLOY_FAILED_KIND: &str = "deploy.failed";
const SOURCES: [&str; 4] = ["github", "drift", "ci", "agent"];

pub fn inbox_list_op(built: &Built, params: Value) -> Result<Value, VogtError> {
    dispatch!(built, inbox_list, params)
}

pub fn inbox_archive_op(built: &Built, params: Value) -> Result<Value, VogtError> {
    dispatch!(built, inbox_archive, params)
}

pub fn inbox_snooze_op(built: &Built, params: Value) -> Result<Value, VogtError> {
    dispatch!(built, inbox_snooze, params)
}

pub fn inbox_restore_op(built: &Built, params: Value) -> Result<Value, VogtError> {
    dispatch!(built, inbox_restore, params)
}

fn inbox_list<C, I>(ctx: &AppContext<C, I>, params: Value) -> Result<Value, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let sources = string_list(&params, "sources")?;
    let triage_states =
        string_list(&params, "triage_states")?.unwrap_or_else(|| vec!["active".to_string()]);
    let actor = params
        .get("actor")
        .and_then(Value::as_str)
        .unwrap_or("any")
        .to_string();
    let project_filter = optional_string(&params, "project")?;
    let work_item_filter = optional_string(&params, "work_item")?;
    let limit = params.get("limit").and_then(Value::as_i64).unwrap_or(50);
    let cursor = optional_string(&params, "cursor")?;

    let now = now_of(&ctx.clock);
    let (declared, observed, _, _, _, config) = context_parts(ctx);
    let view = declared.read()?;

    let mut entries = collect(ctx, observed, &view, config)?;
    let mut project_ids: BTreeMap<String, String> = view
        .list_projects(MAX_SCAN, 0)?
        .into_iter()
        .map(|project| (project.slug, project.id))
        .collect();
    if let Some(slug) = project_filter.as_deref() {
        let project = resolve_project(&view, slug)?;
        project_ids = BTreeMap::from([(slug.to_string(), project.id)]);
    }
    let work_item_id = match work_item_filter.as_deref() {
        Some(reference) => Some(resolve_work_item(&view, reference)?.id),
        None => None,
    };

    let fingerprint = fingerprint(
        sources.as_deref(),
        &triage_states,
        &actor,
        &project_ids,
        work_item_id.as_deref(),
    );
    entries.retain(|entry| {
        (sources
            .as_ref()
            .is_none_or(|wanted| wanted.contains(&text_of(entry, "source"))))
            && project_filter.as_ref().is_none_or(|slug| {
                entry.get("project_slug").and_then(Value::as_str) == Some(slug.as_str())
            })
            && work_item_filter.as_ref().is_none_or(|reference| {
                entry.get("work_item_ref").and_then(Value::as_str) == Some(reference.as_str())
            })
            && triage_matches(entry, &triage_states, now)
    });
    let unknown_hidden = if actor == "external" {
        entries.iter().filter(|entry| actor_unknown(entry)).count() as i64
    } else {
        0
    };
    entries.retain(|entry| actor_matches(entry, &actor));

    let mut source_water = high_water(&entries);
    let cursor_value = match cursor.as_deref() {
        Some(cursor) => Some(decode_cursor(cursor, &fingerprint)?),
        None => None,
    };
    let mut snapshot_at = now;
    if let Some(cursor_value) = cursor_value.as_ref() {
        snapshot_at = cursor_snapshot(cursor_value)?;
        if let Some(cursor_water) = cursor_high_water(cursor_value)? {
            for source in SOURCES {
                source_water.insert(
                    source.to_string(),
                    cursor_water
                        .get(source)
                        .cloned()
                        .map(Value::String)
                        .unwrap_or(Value::Null),
                );
            }
        }
        entries.retain(|entry| within_water(entry, &source_water));
    }
    entries.sort_by_key(|entry| std::cmp::Reverse(sort_key(entry)));
    let start = cursor_index(&entries, cursor_value.as_ref())?;
    let end = (start + limit as usize).min(entries.len());
    let page = &entries[start..end];
    let next_cursor = if end < entries.len() {
        Some(encode_cursor(
            &fingerprint,
            &page[page.len() - 1],
            snapshot_at,
            &source_water,
        )?)
    } else {
        None
    };

    let coverage = coverage_of(observed, &view, &entries, ctx.engine.is_some())?;
    let (engine_status, engine_detail) = engine_status(ctx);
    let counts = json!({
        "active": entries.iter().filter(|entry| state_of(entry) == "active").count(),
        "archived": entries.iter().filter(|entry| state_of(entry) == "archived").count(),
        "snoozed": entries.iter().filter(|entry| state_of(entry) == "snoozed").count(),
    });
    Ok(json!({
        "entries": page,
        "next_cursor": next_cursor,
        "snapshot_at": snapshot_at.to_json(),
        "high_water": ordered_water(&source_water),
        "coverage": coverage,
        "counts": counts,
        "github_scope": "registered projects only",
        "instance_scope": "registered projects only",
        "engine_status": engine_status,
        "engine_detail": engine_detail,
        "engine_available": engine_status == "available",
        "actor_unknown_hidden": unknown_hidden,
    }))
}

fn inbox_archive<C, I>(ctx: &AppContext<C, I>, params: Value) -> Result<Value, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    triage(ctx, &params, TriageState::Archived, None)
}

fn inbox_snooze<C, I>(ctx: &AppContext<C, I>, params: Value) -> Result<Value, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let until_text = require_string(&params, "until")?;
    let until = crate::core::from_iso(&until_text).map_err(|_| {
        VogtError::InvalidSnooze("snooze deadline must be in the future".to_string())
    })?;
    if until <= now_of(&ctx.clock) {
        return Err(VogtError::InvalidSnooze(
            "snooze deadline must be in the future".to_string(),
        ));
    }
    triage(ctx, &params, TriageState::Snoozed, Some(until))
}

fn inbox_restore<C, I>(ctx: &AppContext<C, I>, params: Value) -> Result<Value, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    triage(ctx, &params, TriageState::Active, None)
}

fn triage<C, I>(
    ctx: &AppContext<C, I>,
    params: &Value,
    state: TriageState,
    until: Option<Moment>,
) -> Result<Value, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let entry_key = require_string(params, "entry_key")?;
    let reason = require_string(params, "reason")?;
    let (declared, observed, principal, clock, ids, config) = context_parts(ctx);
    let (entry, existing) = {
        let view = declared.read()?;
        let found = entry_by_key(ctx, observed, &view, config, &entry_key)?;
        let existing = view.inbox_triage_by_key(&entry_key)?;
        (found, existing)
    };
    let Some(entry) = entry else {
        return Err(VogtError::InboxEntryNotFound(format!(
            "no current Inbox entry {}",
            crate::core::py_repr(&entry_key)
        )));
    };
    if let Some(existing) = existing.as_ref() {
        if existing.state == state && state != TriageState::Snoozed {
            return Err(VogtError::InvalidTriageState(format!(
                "Inbox entry {} is already {state}",
                crate::core::py_repr(&entry_key)
            )));
        }
        if state == TriageState::Snoozed && existing.state == TriageState::Archived {
            return Err(VogtError::InvalidTriageState(
                "an archived Inbox entry must be restored before snoozing".to_string(),
            ));
        }
    }
    let operation = match state {
        TriageState::Active => "inbox.restore",
        other => &format!("inbox.{other}"),
    };
    let mut writing = write_context(declared, principal, Arc::clone(&clock), Arc::clone(&ids));
    record_triage(
        &mut writing,
        operation,
        &reason,
        TriageDecision {
            entry_key,
            state,
            until,
            entry,
            decided_at: now_of(&clock),
        },
    )
}

/// What one triage decision records, gathered so the audit helper stays under
/// the argument limit.
struct TriageDecision {
    entry_key: String,
    state: TriageState,
    until: Option<Moment>,
    entry: Value,
    decided_at: Moment,
}

/// Split out so the closure is built against the concrete store. Against the
/// generic `DeclaredStore` bound the compiler treats it as needing a `'static`
/// clock and id factory, which is the same shape `contracts::record_check`
/// avoids.
fn record_triage<C: Clock + 'static, I: IdFactory + 'static>(
    writing: &mut crate::application::writes::WriteContext<'_, C, I, SqliteDeclaredStore<C, I>>,
    operation: &str,
    reason: &str,
    decision: TriageDecision,
) -> Result<Value, VogtError> {
    audited_write(writing, operation, reason, move |txn, actor| {
        let TriageDecision {
            entry_key,
            state,
            until,
            entry,
            decided_at,
        } = decision;
        let triage = InboxTriage {
            entry_key: entry_key.clone(),
            state,
            snooze_until: if state == TriageState::Snoozed {
                until
            } else {
                None
            },
            actor_id: actor.id.clone(),
            actor_identity_ref: Some(actor.identity_ref.clone()),
            decided_at,
            occurrence_snapshot: entry.clone(),
        };
        txn.upsert_inbox_triage(&triage)?;
        let mut updated = entry;
        if let Some(object) = updated.as_object_mut() {
            object.insert("triage_state".to_string(), json!(state.to_string()));
            object.insert(
                "snooze_until".to_string(),
                triage
                    .snooze_until
                    .map(|moment| json!(moment.to_json()))
                    .unwrap_or(Value::Null),
            );
        }
        Ok(WriteOutcome::new(
            json!({"entry": updated}),
            "inbox_triage",
            &entry_key,
            triage_payload(&triage),
            "inbox.triaged",
            json!({"entry_key": entry_key, "state": state.to_string()}),
        ))
    })
}

fn triage_payload(triage: &InboxTriage) -> Value {
    json!({
        "entry_key": triage.entry_key,
        "state": triage.state.to_string(),
        "snooze_until": triage.snooze_until.map(|moment| moment.to_json()),
        "actor_id": triage.actor_id,
        "actor_identity_ref": triage.actor_identity_ref,
        "decided_at": triage.decided_at.to_json(),
        "occurrence_snapshot": triage.occurrence_snapshot,
    })
}

// --- collection -------------------------------------------------------------

fn collect<C, I>(
    ctx: &AppContext<C, I>,
    observed: &crate::storage::sqlite::observed::SqliteObservedStore<C, I>,
    view: &impl ReadView,
    config: &crate::config::VogtConfig,
) -> Result<Vec<Value>, VogtError>
where
    C: Clock,
    I: IdFactory,
{
    let projects: BTreeMap<String, Project> = view
        .list_projects(MAX_SCAN, 0)?
        .into_iter()
        .map(|project| (project.id.clone(), project))
        .collect();
    let bots = actors::normalise_bot_logins(
        &config
            .inbox_bot_logins
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
    );

    let notifications = observed.latest(
        &[crate::adapters::forge::KIND_NOTIFICATION.to_string()],
        None,
        false,
        false,
        MAX_SCAN,
    )?;
    let checks_all = observed.latest(
        &[crate::adapters::forge::KIND_CHECK.to_string()],
        None,
        false,
        false,
        MAX_SCAN,
    )?;
    let task_runs = observed.latest(&[KIND_TASK_RUN.to_string()], None, false, false, MAX_SCAN)?;

    let mut subjects: Vec<String> = notifications
        .iter()
        .chain(checks_all.iter())
        .chain(task_runs.iter())
        .map(|observation| observation.subject_key.clone())
        .collect();
    subjects.sort();
    subjects.dedup();
    let links = view.work_links_for_subjects(&subjects)?;

    let mut entries: Vec<Value> = Vec::new();
    for observation in &notifications {
        let payload = &observation.payload;
        let title = text(payload.get("title"))
            .unwrap_or("GitHub notification")
            .to_string();
        let occurred = when(payload.get("updated_at")).unwrap_or(observation.observed_at);
        let facts = facts_from(payload.get("actor"));
        let actor = match facts {
            Some(facts) => actors::classify(&facts, &bots),
            None => ActorClass {
                login: None,
                kind: None,
                relation: actors::ActorRelation::Unknown,
            },
        };
        entries.push(observation_entry(
            ctx,
            observation,
            &projects,
            &links,
            "github",
            &title,
            text(payload.get("reason")).unwrap_or(&title),
            occurred,
            &actor,
        ));
    }

    for observation in &task_runs {
        let findings = observation
            .payload
            .get("findings")
            .and_then(Value::as_array);
        let Some(findings) = findings.filter(|findings| !findings.is_empty()) else {
            continue;
        };
        let project = observation
            .project_id
            .as_ref()
            .and_then(|id| projects.get(id));
        let title = text(observation.payload.get("task")).unwrap_or("Agent task finding");
        let summary = text(observation.payload.get("summary"))
            .map(str::to_string)
            .or_else(|| {
                findings
                    .first()
                    .and_then(|first| first.as_object())
                    .and_then(|first| text(first.get("text")))
                    .map(str::to_string)
            })
            .unwrap_or_else(|| "An agent task reported a finding.".to_string());
        let material = json!({
            "subject_key": observation.subject_key,
            "findings": findings,
            "summary": summary,
            "status": observation.payload.get("status"),
            "outcome": observation.payload.get("state"),
        });
        let (trust, freshness) = freshness(observation.observed_at, ctx);
        entries.push(entry(EntrySpec {
            entry_key: format!("agent:{}:{}", observation.subject_key, digest_of(&material)),
            source: "agent",
            kind: &observation.kind,
            occurred_at: Some(
                when(observation.payload.get("completed_at"))
                    .or_else(|| when(observation.payload.get("started_at")))
                    .unwrap_or(observation.observed_at),
            ),
            observed_at: Some(observation.observed_at),
            title: &format!("Agent task: {title}"),
            summary: &summary,
            project_slug: project.map(|project| project.slug.as_str()),
            work_item_ref: links.get(&observation.subject_key).map(String::as_str),
            source_subject_key: &observation.subject_key,
            source_url: None,
            trust_state: trust,
            freshness,
            action: action_of(
                "observation",
                json!({"subject_key": observation.subject_key}),
            ),
            actor: &actors::system_actor(),
            extra: json!({}),
        }));
    }

    // One roll-up per project, from the checks already in hand. GitHub-managed
    // runs never make a "CI failing" entry and never move a project's newest
    // revision.
    let repo_checks: Vec<&Observation> = checks_all
        .iter()
        .filter(|observation| !decisions::github_managed(&observation.payload))
        .collect();
    let mut checks_by_project: BTreeMap<&str, Vec<&Observation>> = BTreeMap::new();
    for observation in &repo_checks {
        if let Some(project_id) = observation.project_id.as_deref() {
            checks_by_project
                .entry(project_id)
                .or_default()
                .push(observation);
        }
    }
    let mut newest_revision_ids: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();

    let branch_patterns = config
        .ci_alert_branches
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    let tag_patterns = config
        .ci_alert_tags
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    let mut alerted: BTreeSet<String> = BTreeSet::new();
    for failure in decisions::watched_failures(&checks_all, &branch_patterns, &tag_patterns) {
        if !projects.contains_key(failure.observation.project_id.as_deref().unwrap_or("")) {
            continue;
        }
        alerted.insert(failure.observation.id.clone());
        entries.push(ref_failure_entry(ctx, &failure, &projects, &links));
    }

    for observation in &repo_checks {
        let conclusion = text(observation.payload.get("conclusion"));
        if matches!(conclusion, None | Some("success" | "skipped")) {
            continue;
        }
        if alerted.contains(&observation.id) {
            continue;
        }
        let Some(project) = observation
            .project_id
            .as_ref()
            .and_then(|id| projects.get(id))
        else {
            continue;
        };
        let newest = newest_revision_ids
            .entry(project.id.clone())
            .or_insert_with(|| {
                let owned: Vec<Observation> = checks_by_project
                    .get(project.id.as_str())
                    .map(|checks| checks.iter().map(|check| (*check).clone()).collect())
                    .unwrap_or_default();
                decisions::roll_up(&owned)
                    .map(|rolled| rolled.checks.iter().map(|check| check.id.clone()).collect())
                    .unwrap_or_default()
            });
        if !newest.contains(&observation.id) {
            continue;
        }
        let check = text(observation.payload.get("check")).unwrap_or("CI check");
        let revision = text(observation.payload.get("revision")).unwrap_or("unknown revision");
        entries.push(observation_entry(
            ctx,
            observation,
            &projects,
            &links,
            "ci",
            &format!("CI failing: {check}"),
            &format!("{check} is failing on {revision}"),
            observation.observed_at,
            &actors::system_actor(),
        ));
    }

    // The live engine session list names grant targets and surfaces sessions
    // waiting on a person. An engine that is down or unconfigured leaves both
    // empty, exactly as Python does.
    let live = match &ctx.engine {
        Some(engine) => engine.list_sessions().unwrap_or_default(),
        None => Vec::new(),
    };
    let live_by_id: BTreeMap<&str, &crate::adapters::engine::EngineSession> = live
        .iter()
        .map(|session| (session.id.as_str(), session))
        .collect();

    let mut checks_by_branch: BTreeMap<(String, String), Vec<Observation>> = BTreeMap::new();
    for observation in &checks_all {
        let Some(project_id) = observation.project_id.clone() else {
            continue;
        };
        let Some(branch) = text(observation.payload.get("branch")) else {
            continue;
        };
        if branch.is_empty() {
            continue;
        }
        checks_by_branch
            .entry((project_id, branch.to_string()))
            .or_default()
            .push((*observation).clone());
    }
    for overlay in view.bound_branch_overlays(MAX_BOUND)? {
        let mut settled: Vec<(String, decisions::BranchCi)> = Vec::new();
        for branch in &overlay.branches {
            let Some(runs) = checks_by_branch.get(&(overlay.project_id.clone(), branch.clone()))
            else {
                continue;
            };
            if let Some(ci) = decisions::branch_ci(runs, branch) {
                settled.push((branch.clone(), ci));
            }
        }
        if settled.is_empty() {
            continue;
        }
        let item = view.work_item_by_ref(&overlay.subject_key)?;
        let state = item
            .as_ref()
            .map(|item| item.state.to_string())
            .or(overlay.workflow_state.clone());
        if state
            .as_deref()
            .is_some_and(|state| crate::core::TERMINAL_STATES.contains(&state))
        {
            continue;
        }
        for (branch, ci) in settled {
            if matches!(
                ci.state,
                decisions::BranchState::Passed
                    | decisions::BranchState::Failed
                    | decisions::BranchState::Cancelled
            ) && projects.contains_key(&overlay.project_id)
            {
                entries.push(bound_branch_entry(ctx, &overlay, &branch, &ci, &projects));
            }
        }
    }

    for grant in view.list_session_grants(Some("pending"), None, MAX_SCAN)? {
        if grant.state == crate::core::GrantState::Pending {
            entries.push(grant_entry(view, &grant, &projects, &live_by_id)?);
        }
    }

    for session in &live {
        let declared = view.session_by_engine_id(&session.id)?;
        if let Some(blocked) = &session.blocked {
            if session.alive {
                entries.push(blocked_entry(
                    ctx,
                    session,
                    blocked,
                    declared.as_ref(),
                    &projects,
                    view,
                )?);
            }
        }
        if matches!(
            session.activity.as_str(),
            "waiting-for-input" | "awaiting-approval" | "errored"
        ) {
            entries.push(session_entry(
                ctx,
                session,
                declared.as_ref(),
                &projects,
                view,
            )?);
        }
    }

    if !config.deploy_lanes.is_empty() {
        let lanes = observed.latest(
            &[crate::adapters::forge::KIND_DEPLOY_LANE.to_string()],
            None,
            false,
            false,
            MAX_SCAN,
        )?;
        for lane in &lanes {
            if let Some(entry) = deploy_lane_entry(ctx, lane, &projects) {
                entries.push(entry);
            }
        }
    }

    for proposal in view.list_drift(Some("open"), None, None, MAX_SCAN)? {
        entries.push(drift_entry(view, &proposal, &projects)?);
    }

    let triage = view.inbox_triage_by_keys(
        &entries
            .iter()
            .map(|entry| text_of(entry, "entry_key"))
            .collect::<Vec<_>>(),
    )?;
    let now = now_of(&ctx.clock);
    Ok(entries
        .into_iter()
        .map(|entry| apply_triage(&triage, entry, now))
        .collect())
}

struct EntrySpec<'a> {
    entry_key: String,
    source: &'a str,
    kind: &'a str,
    occurred_at: Option<Moment>,
    observed_at: Option<Moment>,
    title: &'a str,
    summary: &'a str,
    project_slug: Option<&'a str>,
    work_item_ref: Option<&'a str>,
    source_subject_key: &'a str,
    source_url: Option<&'a str>,
    trust_state: String,
    freshness: &'a str,
    action: Value,
    actor: &'a ActorClass,
    extra: Value,
}

fn entry(spec: EntrySpec<'_>) -> Value {
    let mut value = json!({
        "entry_key": spec.entry_key,
        "source": spec.source,
        "kind": spec.kind,
        "occurred_at": spec.occurred_at.map(|moment| moment.to_json()),
        "observed_at": spec.observed_at.map(|moment| moment.to_json()),
        "title": spec.title,
        "summary": spec.summary,
        "project_slug": spec.project_slug,
        "work_item_ref": spec.work_item_ref,
        "session_id": Value::Null,
        "source_subject_key": spec.source_subject_key,
        "source_url": spec.source_url,
        "trust_state": spec.trust_state,
        "freshness": spec.freshness,
        "provisional": false,
        "triage_state": "active",
        "snooze_until": Value::Null,
        "action": spec.action,
        "evidence_snapshot": Value::Null,
        "proposed_change": Value::Null,
        "actor_login": spec.actor.login,
        "actor_kind": spec.actor.kind.map(|kind| match kind {
            actors::ActorKind::Human => "human",
            actors::ActorKind::Bot => "bot",
        }),
        "actor_relation": match spec.actor.relation {
            actors::ActorRelation::OrgMember => "org_member",
            actors::ActorRelation::External => "external",
            actors::ActorRelation::Unknown => "unknown",
        },
    });
    if let (Some(object), Some(extra)) = (value.as_object_mut(), spec.extra.as_object()) {
        for (key, extra_value) in extra {
            object.insert(key.clone(), extra_value.clone());
        }
    }
    value
}

#[allow(clippy::too_many_arguments)]
fn observation_entry<C: Clock, I: IdFactory>(
    ctx: &AppContext<C, I>,
    observation: &Observation,
    projects: &BTreeMap<String, Project>,
    links: &BTreeMap<String, String>,
    source: &str,
    title: &str,
    summary: &str,
    occurred: Moment,
    actor: &ActorClass,
) -> Value {
    let project = observation
        .project_id
        .as_ref()
        .and_then(|id| projects.get(id));
    let mut payload = observation.payload.as_object().cloned().unwrap_or_default();
    for key in ["unread", "last_read", "observed_at", "actor"] {
        payload.remove(key);
    }
    let material = json!({
        "subject_key": observation.subject_key,
        "title": title,
        "summary": summary,
        "source_url": observation.source_url,
        "payload": payload,
    });
    let (trust, freshness) = freshness(observation.observed_at, ctx);
    entry(EntrySpec {
        entry_key: format!(
            "{source}:{}:{}",
            observation.subject_key,
            digest_of(&material)
        ),
        source,
        kind: &observation.kind,
        occurred_at: Some(occurred),
        observed_at: Some(observation.observed_at),
        title,
        summary,
        project_slug: project.map(|project| project.slug.as_str()),
        work_item_ref: links.get(&observation.subject_key).map(String::as_str),
        source_subject_key: &observation.subject_key,
        source_url: observation.source_url.as_deref(),
        trust_state: trust,
        freshness,
        action: action_of(
            "observation",
            json!({"subject_key": observation.subject_key}),
        ),
        actor,
        extra: json!({}),
    })
}

fn ref_failure_entry<C: Clock, I: IdFactory>(
    ctx: &AppContext<C, I>,
    failure: &decisions::RefFailure,
    projects: &BTreeMap<String, Project>,
    links: &BTreeMap<String, String>,
) -> Value {
    let observation = &failure.observation;
    let payload = &observation.payload;
    let project = observation
        .project_id
        .as_ref()
        .and_then(|id| projects.get(id));
    let jobs: Vec<&serde_json::Map<String, Value>> = payload
        .get("failed_jobs")
        .and_then(Value::as_array)
        .map(|jobs| {
            jobs.iter()
                .filter_map(Value::as_object)
                .filter(|job| job.get("name").and_then(Value::as_str).is_some())
                .collect()
        })
        .unwrap_or_default();
    let revision = text(payload.get("revision")).unwrap_or("unknown revision");
    let lane = if failure.lane.kind == decisions::LaneKind::Branch {
        format!("a later run on {}", failure.lane.reference)
    } else {
        format!("a later run on a tag matching {}", failure.lane.lane)
    };
    let what = if jobs.is_empty() {
        format!("The run concluded {}.", failure.conclusion)
    } else {
        let named = jobs
            .iter()
            .take(3)
            .map(|job| job["name"].as_str().unwrap_or(""))
            .collect::<Vec<_>>()
            .join(", ");
        let more = if jobs.len() > 3 {
            format!(" (+{} more)", jobs.len() - 3)
        } else {
            String::new()
        };
        format!("Failed job: {named}{more}.")
    };
    let short = &revision[..revision.len().min(12)];
    let summary = format!(
        "{what} {} on {} at {short}. Clears when {lane} of {} succeeds.",
        failure.workflow, failure.lane.reference, failure.workflow
    );
    let log_url = jobs
        .iter()
        .find_map(|job| {
            job.get("url")
                .and_then(Value::as_str)
                .filter(|url| !url.is_empty())
        })
        .or(observation.source_url.as_deref());
    let material = json!({
        "subject_key": observation.subject_key,
        "run_id": payload.get("run_id"),
        "run_attempt": payload.get("run_attempt"),
        "run_number": payload.get("run_number"),
        "conclusion": failure.conclusion,
    });
    let (trust, freshness) = freshness(observation.observed_at, ctx);
    entry(EntrySpec {
        entry_key: format!(
            "ci:ref:{}:{}",
            observation.subject_key,
            digest_of(&material)
        ),
        source: "ci",
        kind: REF_FAILURE_KIND,
        occurred_at: Some(when(payload.get("updated_at")).unwrap_or(observation.observed_at)),
        observed_at: Some(observation.observed_at),
        title: &format!("{} failed on {}", failure.workflow, failure.lane.reference),
        summary: &truncate(&summary, 1000),
        project_slug: project.map(|project| project.slug.as_str()),
        work_item_ref: links.get(&observation.subject_key).map(String::as_str),
        source_subject_key: &observation.subject_key,
        source_url: log_url,
        trust_state: trust,
        freshness,
        action: action_of(
            "observation",
            json!({"subject_key": observation.subject_key}),
        ),
        actor: &actors::system_actor(),
        extra: json!({}),
    })
}

fn deploy_lane_entry<C: Clock, I: IdFactory>(
    ctx: &AppContext<C, I>,
    observation: &Observation,
    projects: &BTreeMap<String, Project>,
) -> Option<Value> {
    let receipt = observation.payload.get("receipt")?.as_object()?;
    if receipt.get("status").and_then(Value::as_str) != Some("failed") {
        return None;
    }
    let project = observation
        .project_id
        .as_ref()
        .and_then(|id| projects.get(id))?;
    let lane = text(observation.payload.get("lane")).unwrap_or("deploy");
    let sha = text(receipt.get("source_sha")).unwrap_or("unknown revision");
    let tag = text(receipt.get("source_tag"));
    let material = json!({"subject_key": observation.subject_key, "receipt": receipt});
    let (trust, freshness) = freshness(observation.observed_at, ctx);
    Some(entry(EntrySpec {
        entry_key: format!("ci:deploy:{}:{}", observation.subject_key, digest_of(&material)),
        source: "ci",
        kind: DEPLOY_FAILED_KIND,
        occurred_at: Some(when(receipt.get("timestamp")).unwrap_or(observation.observed_at)),
        observed_at: Some(observation.observed_at),
        title: &format!("Deploy failed on lane {lane}"),
        summary: &format!(
            "The {lane} receipt reports a failed deploy of {}. Clears when a later receipt reports success.",
            tag.unwrap_or(&sha[..sha.len().min(12)])
        ),
        project_slug: Some(&project.slug),
        work_item_ref: None,
        source_subject_key: &observation.subject_key,
        source_url: text(receipt.get("url")).or(observation.source_url.as_deref()),
        trust_state: trust,
        freshness,
        action: action_of("observation", json!({"subject_key": observation.subject_key})),
        actor: &actors::system_actor(),
        extra: json!({}),
    }))
}

/// An action carries every target key, the unused ones null. Python's model
/// renders its defaults, and a snapshot that dropped them would persist a
/// different shape.
fn action_of(kind: &str, fields: Value) -> Value {
    let mut action = json!({
        "kind": kind,
        "drift_id": Value::Null,
        "subject_key": Value::Null,
        "session_id": Value::Null,
        "grant_id": Value::Null,
    });
    if let (Some(object), Some(extra)) = (action.as_object_mut(), fields.as_object()) {
        for (key, value) in extra {
            object.insert(key.clone(), value.clone());
        }
    }
    action
}

/// CI settled on a branch a work item is bound to — pass or fail.
fn bound_branch_entry<C: Clock, I: IdFactory>(
    ctx: &AppContext<C, I>,
    overlay: &crate::core::WorkOverlay,
    branch: &str,
    ci: &decisions::BranchCi,
    projects: &BTreeMap<String, Project>,
) -> Value {
    let project = projects.get(&overlay.project_id);
    let newest = ci
        .runs
        .iter()
        .max_by_key(|run| run.observation.observed_at)
        .expect("a settled branch has runs");
    let failing_url = ci.runs.iter().find_map(|run| {
        (ci.failing.contains(&run.workflow))
            .then(|| run.observation.source_url.clone())
            .flatten()
    });
    let state = match ci.state {
        decisions::BranchState::Failed => "failed",
        decisions::BranchState::Passed => "passed",
        decisions::BranchState::Cancelled => "cancelled",
        decisions::BranchState::Running => "running",
    };
    let head = match ci.state {
        decisions::BranchState::Failed => format!("Failing: {}.", ci.failing.join(", ")),
        decisions::BranchState::Passed => format!("All {} workflow(s) passed.", ci.runs.len()),
        _ => "Every run on the revision was cancelled.".to_string(),
    };
    let short = if ci.revision.len() > 12 {
        &ci.revision[..12]
    } else {
        &ci.revision
    };
    let summary = truncate(
        &format!("{head} {branch} @ {short} for {}.", overlay.subject_key),
        1000,
    );
    let material = json!({
        "branch": branch,
        "revision": ci.revision,
        "state": state,
        "runs": ci.runs.iter().map(|run| json!([run.workflow, run.conclusion])).collect::<Vec<_>>(),
    });
    let (trust, freshness) = freshness(newest.observation.observed_at, ctx);
    let subject = format!("ci-branch:{}:{branch}", overlay.subject_key);
    let occurred = ci
        .concluded_at
        .as_deref()
        .and_then(|text| crate::core::from_iso(text).ok())
        .unwrap_or(newest.observation.observed_at);
    entry(EntrySpec {
        entry_key: format!("ci:branch:{subject}:{}", digest_of(&material)),
        source: "ci",
        kind: "ci.branch_concluded",
        occurred_at: Some(occurred),
        observed_at: Some(newest.observation.observed_at),
        title: &format!("CI {state} on {branch}"),
        summary: &summary,
        project_slug: project.map(|project| project.slug.as_str()),
        work_item_ref: Some(&overlay.subject_key),
        source_subject_key: &newest.observation.subject_key,
        source_url: failing_url
            .as_deref()
            .or(newest.observation.source_url.as_deref()),
        trust_state: trust,
        freshness,
        action: action_of(
            "observation",
            json!({"subject_key": newest.observation.subject_key}),
        ),
        actor: &actors::system_actor(),
        extra: json!({}),
    })
}

/// A grant a session asked for and a person has yet to decide. The target is
/// named as the person knows it — title, role, agent, project — because
/// approving the right secret for the wrong session is the mistake this entry
/// must not invite. A target absent from the live list is the "not running
/// now" entry.
fn grant_entry(
    view: &impl ReadView,
    grant: &crate::core::SessionGrant,
    projects: &BTreeMap<String, Project>,
    live: &BTreeMap<&str, &crate::adapters::engine::EngineSession>,
) -> Result<Value, VogtError> {
    let target = live.get(grant.target_engine_session_id.as_str()).copied();
    let declared = view.session_by_engine_id(&grant.target_engine_session_id)?;
    let project = declared
        .as_ref()
        .and_then(|session| projects.get(&session.project_id));
    let label = target
        .filter(|session| !session.name.is_empty())
        .map(|session| session.name.as_str())
        .unwrap_or(grant.target_engine_session_id.as_str());
    let mut facts: Vec<String> = Vec::new();
    if let Some(target) = target {
        facts.push(target.role.clone());
        if let Some(agent) = &target.conversation_agent {
            facts.push(agent.clone());
        }
        if let Some(mode) = &target.permission_mode {
            facts.push(format!("permission {mode}"));
        }
    } else {
        facts.push("not running now".to_string());
    }
    if let Some(project) = project {
        facts.push(format!("project {}", project.slug));
    }
    if let Some(session) = declared.as_ref() {
        facts.push(session.id.clone());
    }
    let requester = view
        .actor_by_id(&grant.requested_by)?
        .or(view.actor_by_identity(&grant.requested_by)?);
    let requested_by = requester
        .as_ref()
        .map(|actor| actor.identity_ref.clone())
        .unwrap_or_else(|| grant.requested_by.clone());
    let itself = requested_by == format!("agent:engine:{}", grant.target_engine_session_id)
        || declared
            .as_ref()
            .is_some_and(|session| requested_by == format!("agent:session:{}", session.id));
    let who = if itself {
        "itself".to_string()
    } else {
        format!("session {label} ({})", facts.join(", "))
    };
    let item = format!(
        "{} (project {}) as {}",
        py_none(grant.secret_name.as_deref()),
        py_none(grant.project_id.as_deref()),
        py_none(grant.var.as_deref())
    );
    let uses = if grant.uses.to_string() == "once" {
        "one fetch"
    } else {
        "any number of fetches"
    };
    let summary = truncate(
        &format!(
            "{requested_by} asks for {item} for {who}: {uses}, for {} min once approved. Reason: {}",
            grant.ttl_seconds / 60,
            grant.reason
        ),
        1000,
    );
    let actor = ActorClass {
        login: Some(requested_by.clone()),
        kind: Some(match &requester {
            Some(actor) if actor.kind == crate::core::ActorKind::Human => actors::ActorKind::Human,
            _ => actors::ActorKind::Bot,
        }),
        relation: actors::ActorRelation::OrgMember,
    };
    Ok(entry(EntrySpec {
        entry_key: format!("agent:grant:{}", grant.id),
        source: "agent",
        kind: "session.grant_request",
        occurred_at: Some(grant.requested_at),
        observed_at: None,
        title: &format!(
            "Grant request: {} for session {label}",
            py_none(grant.secret_name.as_deref())
        ),
        summary: &summary,
        project_slug: project.map(|project| project.slug.as_str()),
        work_item_ref: None,
        source_subject_key: &grant.id,
        source_url: None,
        trust_state: "unverified".to_string(),
        freshness: "live",
        action: action_of(
            "grant",
            json!({"grant_id": grant.id, "session_id": grant.target_engine_session_id}),
        ),
        actor: &actor,
        extra: json!({
            "session_id": grant.target_engine_session_id,
            "provisional": true,
            "evidence_snapshot": {
                "grant_id": grant.id,
                "target": grant.target_engine_session_id,
                "target_name": target.map(|session| session.name.clone()),
                "target_role": target.map(|session| session.role.clone()),
                "target_agent": target.and_then(|session| session.conversation_agent.clone()),
                "target_permission_mode": target.and_then(|session| session.permission_mode.clone()),
                "target_alive": target.is_some(),
                "target_session": declared.as_ref().map(|session| session.id.clone()),
                "project": project.map(|project| project.slug.clone()),
                "requested_by": requested_by,
                "requester_is_target": itself,
                "var": grant.var,
                "project_id": grant.project_id,
                "secret_name": grant.secret_name,
                "uses": grant.uses.to_string(),
                "ttl_seconds": grant.ttl_seconds,
            },
        }),
    }))
}

/// Python renders a missing optional as the literal `None`.
fn py_none(value: Option<&str>) -> &str {
    value.unwrap_or("None")
}

/// A session whose agent reported it cannot go on without a person. Keyed by
/// the report's time, so a new report after an archived one surfaces again.
fn blocked_entry<C: Clock, I: IdFactory>(
    ctx: &AppContext<C, I>,
    session: &crate::adapters::engine::EngineSession,
    blocked: &crate::adapters::engine::EngineBlocked,
    declared: Option<&crate::core::CodingSession>,
    projects: &BTreeMap<String, Project>,
    view: &impl ReadView,
) -> Result<Value, VogtError> {
    let project = declared.and_then(|session| projects.get(&session.project_id));
    let reference = bound_ref(view, declared, session.work_item.as_deref())?;
    let todo = blocked.items.join("; ");
    let summary = truncate(
        &format!(
            "{}{}",
            blocked.reason,
            if todo.is_empty() {
                String::new()
            } else {
                format!(" — to do: {todo}")
            }
        ),
        1000,
    );
    let name = if session.name.is_empty() {
        session.id.as_str()
    } else {
        session.name.as_str()
    };
    let title = match &reference {
        Some(reference) => format!("{reference} session {name} is blocked on you"),
        None => format!("Session {name} is blocked on you"),
    };
    Ok(entry(EntrySpec {
        entry_key: format!(
            "agent:session:{}:blocked:{}",
            session.id,
            blocked.since.as_deref().unwrap_or("unknown")
        ),
        source: "agent",
        kind: "session.blocked",
        occurred_at: Some(
            when(blocked.since.as_deref().map(Value::from).as_ref())
                .unwrap_or_else(|| now_of(&ctx.clock)),
        ),
        observed_at: None,
        title: &title,
        summary: &summary,
        project_slug: project.map(|project| project.slug.as_str()),
        work_item_ref: reference.as_deref(),
        source_subject_key: &session.id,
        source_url: None,
        trust_state: "unverified".to_string(),
        freshness: "live",
        action: action_of("session", json!({"session_id": session.id})),
        actor: &actors::system_actor(),
        extra: json!({"session_id": session.id, "provisional": true}),
    }))
}

/// A session waiting on a person: an approval dialog, input, or an error.
fn session_entry<C: Clock, I: IdFactory>(
    ctx: &AppContext<C, I>,
    session: &crate::adapters::engine::EngineSession,
    declared: Option<&crate::core::CodingSession>,
    projects: &BTreeMap<String, Project>,
    view: &impl ReadView,
) -> Result<Value, VogtError> {
    let project = declared.and_then(|session| projects.get(&session.project_id));
    let reference = bound_ref(view, declared, session.work_item.as_deref())?;
    let name = if session.name.is_empty() {
        session.id.as_str()
    } else {
        session.name.as_str()
    };
    let label = match &reference {
        Some(reference) => format!("{reference} {name}"),
        None => name.to_string(),
    };
    let (title, summary) = if session.activity == "awaiting-approval" {
        let what = session
            .approval
            .as_ref()
            .map(|approval| {
                approval
                    .command_excerpt
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .unwrap_or_default();
        let left = session
            .approval
            .as_ref()
            .and_then(|approval| approval.deadline_seconds)
            .map(|seconds| format!(" Auto-deny in {seconds}s."))
            .unwrap_or_default();
        let question = session
            .approval
            .as_ref()
            .map(|approval| approval.question.as_str())
            .unwrap_or("Permission dialog.");
        (
            format!("Session {label} is asking for approval"),
            truncate(format!("{question}{left} {what}").trim(), 1000),
        )
    } else {
        (
            format!("Session {label} needs attention"),
            format!("Session is {}.", session.activity),
        )
    };
    Ok(entry(EntrySpec {
        entry_key: format!(
            "agent:session:{}:{}:{}",
            session.id,
            session.activity,
            session.activity_changed_at.as_deref().unwrap_or("unknown")
        ),
        source: "agent",
        kind: "session.attention",
        occurred_at: Some(
            when(
                session
                    .activity_changed_at
                    .as_deref()
                    .map(Value::from)
                    .as_ref(),
            )
            .unwrap_or_else(|| now_of(&ctx.clock)),
        ),
        observed_at: None,
        title: &title,
        summary: &summary,
        project_slug: project.map(|project| project.slug.as_str()),
        work_item_ref: reference.as_deref(),
        source_subject_key: &session.id,
        source_url: None,
        trust_state: "unverified".to_string(),
        freshness: "live",
        action: action_of("session", json!({"session_id": session.id})),
        actor: &actors::system_actor(),
        extra: json!({"session_id": session.id, "provisional": true}),
    }))
}

/// The work item a session serves: the core's row for a session Vogt started,
/// the engine's label for one the GUI started.
fn bound_ref(
    view: &impl ReadView,
    declared: Option<&crate::core::CodingSession>,
    label: Option<&str>,
) -> Result<Option<String>, VogtError> {
    if let Some(declared) = declared {
        let Some(work_item_id) = &declared.work_item_id else {
            return Ok(None);
        };
        return Ok(view
            .work_item_by_id(work_item_id)?
            .map(|item| item.reference));
    }
    Ok(label.map(str::to_string))
}

fn drift_entry(
    view: &impl ReadView,
    proposal: &DriftProposal,
    projects: &BTreeMap<String, Project>,
) -> Result<Value, VogtError> {
    let project = proposal.project_id.as_ref().and_then(|id| projects.get(id));
    let reference = if proposal.subject_kind == "work_item" {
        view.work_item_by_id(&proposal.subject_id)?
            .map(|item| item.reference)
    } else {
        None
    };
    let material = digest_of(&json!({
        "evidence": proposal.evidence_snapshot,
        "proposed": proposal.proposed_change,
    }));
    Ok(entry(EntrySpec {
        entry_key: format!("drift:{}:{material}", proposal.id),
        source: "drift",
        kind: &proposal.kind,
        occurred_at: Some(proposal.opened_at),
        observed_at: Some(proposal.opened_at),
        title: &proposal.summary,
        summary: &proposal.summary,
        project_slug: project.map(|project| project.slug.as_str()),
        work_item_ref: reference.as_deref(),
        source_subject_key: &proposal.id,
        source_url: None,
        trust_state: "disputed".to_string(),
        freshness: "current",
        action: action_of("drift", json!({"drift_id": proposal.id})),
        actor: &actors::system_actor(),
        extra: json!({
            "evidence_snapshot": proposal.evidence_snapshot,
            "proposed_change": proposal.proposed_change,
        }),
    }))
}

/// `views.trust_for`: verified while the observation is inside the verify
/// horizon, stale past it. Freshness is current only when verified.
fn freshness<C: Clock, I: IdFactory>(
    observed_at: Moment,
    ctx: &AppContext<C, I>,
) -> (String, &'static str) {
    let horizon = ctx.config.verify_horizon_hours * 3600;
    let trust = if now_of(&ctx.clock).unix_seconds() - observed_at.unix_seconds() <= horizon {
        "verified"
    } else {
        "stale"
    };
    let freshness = if trust == "verified" {
        "current"
    } else {
        "stale"
    };
    (trust.to_string(), freshness)
}

fn apply_triage(decisions: &BTreeMap<String, InboxTriage>, mut entry: Value, now: Moment) -> Value {
    let Some(triage) = entry
        .get("entry_key")
        .and_then(Value::as_str)
        .and_then(|key| decisions.get(key))
    else {
        return entry;
    };
    if triage.state == TriageState::Snoozed && triage.snooze_until.is_some_and(|until| until <= now)
    {
        return entry;
    }
    if let Some(object) = entry.as_object_mut() {
        object.insert("triage_state".to_string(), json!(triage.state.to_string()));
        object.insert(
            "snooze_until".to_string(),
            triage
                .snooze_until
                .map(|moment| json!(moment.to_json()))
                .unwrap_or(Value::Null),
        );
    }
    entry
}

fn entry_by_key<C, I>(
    ctx: &AppContext<C, I>,
    observed: &crate::storage::sqlite::observed::SqliteObservedStore<C, I>,
    view: &impl ReadView,
    config: &crate::config::VogtConfig,
    key: &str,
) -> Result<Option<Value>, VogtError>
where
    C: Clock,
    I: IdFactory,
{
    Ok(collect(ctx, observed, view, config)?
        .into_iter()
        .find(|entry| entry.get("entry_key").and_then(Value::as_str) == Some(key)))
}

// --- filtering, ordering, cursors ------------------------------------------

fn actor_matches(entry: &Value, wanted: &str) -> bool {
    let actor = ActorClass {
        login: entry
            .get("actor_login")
            .and_then(Value::as_str)
            .map(str::to_string),
        kind: match entry.get("actor_kind").and_then(Value::as_str) {
            Some("human") => Some(actors::ActorKind::Human),
            Some("bot") => Some(actors::ActorKind::Bot),
            _ => None,
        },
        relation: match entry.get("actor_relation").and_then(Value::as_str) {
            Some("org_member") => actors::ActorRelation::OrgMember,
            Some("external") => actors::ActorRelation::External,
            _ => actors::ActorRelation::Unknown,
        },
    };
    actors::matches(&actor, wanted)
}

fn actor_unknown(entry: &Value) -> bool {
    entry.get("actor_kind").and_then(Value::as_str) != Some("bot")
        && entry.get("actor_relation").and_then(Value::as_str) == Some("unknown")
}

fn triage_matches(entry: &Value, states: &[String], now: Moment) -> bool {
    let mut state = state_of(entry);
    if state == "snoozed" {
        if let Some(until) = entry
            .get("snooze_until")
            .and_then(Value::as_str)
            .and_then(parse_moment)
        {
            if until <= now {
                state = "active".to_string();
            }
        }
    }
    states.iter().any(|wanted| wanted == &state)
}

fn sort_key(entry: &Value) -> (String, String) {
    let moment = entry
        .get("occurred_at")
        .and_then(Value::as_str)
        .or_else(|| entry.get("observed_at").and_then(Value::as_str))
        .unwrap_or("0001-01-01T00:00:00Z")
        .to_string();
    (moment, text_of(entry, "entry_key"))
}

fn fingerprint(
    sources: Option<&[String]>,
    states: &[String],
    actor: &str,
    project_ids: &BTreeMap<String, String>,
    work_item_id: Option<&str>,
) -> String {
    let mut material = json!({
        "sources": sources,
        "states": states,
        "project": project_ids.keys().collect::<Vec<_>>(),
        "work_item": work_item_id,
    });
    if actor != "any" {
        material
            .as_object_mut()
            .expect("object")
            .insert("actor".to_string(), json!(actor));
    }
    digest_of(&material)
}

fn encode_cursor(
    fingerprint: &str,
    entry: &Value,
    snapshot_at: Moment,
    high_water: &BTreeMap<String, Value>,
) -> Result<String, VogtError> {
    let moment = entry_moment(entry)?;
    let raw = serde_json::to_vec(&json!({
        "fingerprint": fingerprint,
        "occurred_at": moment.to_json(),
        "entry_key": text_of(entry, "entry_key"),
        "snapshot_at": snapshot_at.to_json(),
        "high_water": high_water.iter().map(|(source, value)| {
            (source.clone(), value.as_str().and_then(parse_moment).map(|moment| moment.to_json()))
        }).collect::<BTreeMap<_, _>>(),
    }))
    .expect("cursor material is json");
    Ok(base64_encode(&raw).trim_end_matches('=').to_string())
}

fn decode_cursor(cursor: &str, fingerprint: &str) -> Result<Value, VogtError> {
    let invalid = || {
        VogtError::InvalidCursor(
            "cursor is malformed or belongs to another Inbox query".to_string(),
        )
    };
    let raw = base64_decode(cursor).map_err(|_| invalid())?;
    let value: Value = serde_json::from_slice(&raw).map_err(|_| invalid())?;
    let object = value.as_object().ok_or_else(invalid)?;
    if object.get("fingerprint").and_then(Value::as_str) != Some(fingerprint) {
        return Err(invalid());
    }
    let occurred = object
        .get("occurred_at")
        .and_then(Value::as_str)
        .ok_or_else(invalid)?;
    if object.get("entry_key").and_then(Value::as_str).is_none() {
        return Err(invalid());
    }
    crate::core::from_iso(occurred).map_err(|_| invalid())?;
    let snapshot = object
        .get("snapshot_at")
        .and_then(Value::as_str)
        .ok_or_else(invalid)?;
    crate::core::from_iso(snapshot).map_err(|_| invalid())?;
    Ok(value)
}

fn cursor_index(entries: &[Value], cursor: Option<&Value>) -> Result<usize, VogtError> {
    let Some(cursor) = cursor else {
        return Ok(0);
    };
    let invalid = || {
        VogtError::InvalidCursor(
            "cursor is malformed or belongs to another Inbox query".to_string(),
        )
    };
    let occurred = cursor
        .get("occurred_at")
        .and_then(Value::as_str)
        .ok_or_else(invalid)?;
    let key = cursor
        .get("entry_key")
        .and_then(Value::as_str)
        .ok_or_else(invalid)?;
    let boundary = (occurred.to_string(), key.to_string());
    Ok(entries
        .iter()
        .position(|entry| sort_key(entry) < boundary)
        .unwrap_or(entries.len()))
}

fn cursor_high_water(cursor: &Value) -> Result<Option<BTreeMap<String, String>>, VogtError> {
    let Some(raw) = cursor.get("high_water") else {
        return Ok(None);
    };
    let object = raw.as_object().ok_or_else(|| {
        VogtError::InvalidCursor("cursor does not carry a high-water mark".to_string())
    })?;
    let mut result = BTreeMap::new();
    for source in SOURCES {
        let Some(value) = object.get(source) else {
            continue;
        };
        if value.is_null() {
            continue;
        }
        let text = value.as_str().ok_or_else(|| {
            VogtError::InvalidCursor("cursor has an invalid high-water mark".to_string())
        })?;
        crate::core::from_iso(text).map_err(|_| {
            VogtError::InvalidCursor("cursor has an invalid high-water mark".to_string())
        })?;
        result.insert(source.to_string(), text.to_string());
    }
    Ok(Some(result))
}

fn cursor_snapshot(cursor: &Value) -> Result<Moment, VogtError> {
    let text = cursor
        .get("snapshot_at")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            VogtError::InvalidCursor("cursor has an invalid snapshot time".to_string())
        })?;
    crate::core::from_iso(text)
        .map_err(|_| VogtError::InvalidCursor("cursor has an invalid snapshot time".to_string()))
}

fn ordered_water(water: &BTreeMap<String, Value>) -> serde_json::Map<String, Value> {
    let mut ordered = serde_json::Map::new();
    for source in SOURCES {
        ordered.insert(
            source.to_string(),
            water.get(source).cloned().unwrap_or(Value::Null),
        );
    }
    ordered
}

fn high_water(entries: &[Value]) -> BTreeMap<String, Value> {
    let mut result: BTreeMap<String, Value> = SOURCES
        .iter()
        .map(|source| (source.to_string(), Value::Null))
        .collect();
    for entry in entries {
        let Some(moment) = entry
            .get("occurred_at")
            .and_then(Value::as_str)
            .or_else(|| entry.get("observed_at").and_then(Value::as_str))
        else {
            continue;
        };
        let source = text_of(entry, "source");
        let previous = result.get(&source).and_then(Value::as_str);
        if previous.is_none_or(|previous| moment > previous) {
            result.insert(source, json!(moment));
        }
    }
    result
}

fn within_water(entry: &Value, water: &BTreeMap<String, Value>) -> bool {
    let Some(high) = water.get(&text_of(entry, "source")).and_then(Value::as_str) else {
        return false;
    };
    entry_moment(entry).is_ok_and(|moment| moment.to_json().as_str() <= high)
}

fn coverage_of<C, I>(
    observed: &crate::storage::sqlite::observed::SqliteObservedStore<C, I>,
    view: &impl ReadView,
    entries: &[Value],
    engine_configured: bool,
) -> Result<Value, VogtError>
where
    C: Clock,
    I: IdFactory,
{
    let registered = view.list_projects(MAX_SCAN, 0)?.len() as i64;
    let sweeps = observed.coverage()?;
    let mut result = serde_json::Map::new();
    for (source, collector) in [
        ("github", crate::adapters::forge::COLLECTOR_NOTIFICATIONS),
        ("ci", crate::adapters::forge::COLLECTOR_CHECKS),
    ] {
        result.insert(
            source.to_string(),
            coverage_row(source, sweeps.get(collector), entries, registered),
        );
    }
    result.insert(
        "drift".to_string(),
        json!({
            "source": "drift",
            "status": "current",
            "count": entries.iter().filter(|entry| text_of(entry, "source") == "drift").count(),
            "observed_at": Value::Null,
            "projects": 0,
            "registered": registered,
            "detail": "open proposals in the declared store",
        }),
    );
    result.insert(
        "agent".to_string(),
        json!({
            "source": "agent",
            "status": if engine_configured { "current" } else { "unconfigured" },
            "count": entries.iter().filter(|entry| text_of(entry, "source") == "agent").count(),
            "observed_at": Value::Null,
            "projects": 0,
            "registered": registered,
            "detail": if engine_configured { Value::Null } else { json!("no session engine is configured") },
        }),
    );
    Ok(Value::Object(result))
}

fn coverage_row(source: &str, sweep: Option<&Sweep>, entries: &[Value], registered: i64) -> Value {
    json!({
        "source": source,
        "status": sweep.map(|sweep| sweep.outcome.to_string()).unwrap_or_else(|| "unswept".to_string()),
        "count": entries.iter().filter(|entry| text_of(entry, "source") == source).count(),
        "observed_at": sweep.and_then(|sweep| sweep.finished_at).map(|moment| moment.to_json()),
        "projects": 0,
        "registered": registered,
        "detail": match sweep {
            Some(_) => Value::Null,
            None => json!("this collector has not completed a sweep"),
        },
    })
}

fn engine_status<C, I>(ctx: &AppContext<C, I>) -> (&'static str, Option<String>)
where
    C: Clock,
    I: IdFactory,
{
    let Some(engine) = ctx.engine.as_ref() else {
        return (
            "not_configured",
            Some("no session engine is configured".to_string()),
        );
    };
    match engine.list_sessions() {
        Ok(_) => ("available", None),
        Err(error) => ("unreachable", Some(error.to_string())),
    }
}

fn entry_moment(entry: &Value) -> Result<Moment, VogtError> {
    entry
        .get("occurred_at")
        .and_then(Value::as_str)
        .or_else(|| entry.get("observed_at").and_then(Value::as_str))
        .and_then(parse_moment)
        .ok_or_else(|| VogtError::InvalidCursor("Inbox entry has no timestamp".to_string()))
}

// --- small helpers ----------------------------------------------------------

fn text(value: Option<&Value>) -> Option<&str> {
    value
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
}

fn text_of(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

fn state_of(entry: &Value) -> String {
    entry
        .get("triage_state")
        .and_then(Value::as_str)
        .unwrap_or("active")
        .to_string()
}

fn when(value: Option<&Value>) -> Option<Moment> {
    text(value).and_then(parse_moment)
}

fn parse_moment(text: &str) -> Option<Moment> {
    crate::core::from_iso(&text.replace('Z', "+00:00"))
        .ok()
        .or_else(|| crate::core::from_iso(text).ok())
}

fn facts_from(raw: Option<&Value>) -> Option<actors::ActorFacts> {
    raw.and_then(actors::facts_from_payload)
}

fn truncate(text: &str, limit: usize) -> String {
    text.chars().take(limit).collect()
}

fn string_list(params: &Value, key: &str) -> Result<Option<Vec<String>>, VogtError> {
    match params.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Array(values)) => Ok(Some(
            values
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect(),
        )),
        Some(_) => Err(VogtError::InvalidRequest(format!("{key} must be a list"))),
    }
}

fn require_string(params: &Value, key: &str) -> Result<String, VogtError> {
    params
        .get(key)
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
        .ok_or_else(|| VogtError::InvalidRequest(format!("{key} is required")))
}

fn optional_string(params: &Value, key: &str) -> Result<Option<String>, VogtError> {
    match params.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => Ok(Some(text.clone())),
        Some(_) => Err(VogtError::InvalidRequest(format!("{key} must be a string"))),
    }
}

fn resolve_project(view: &impl ReadView, slug: &str) -> Result<Project, VogtError> {
    view.list_projects(MAX_SCAN, 0)?
        .into_iter()
        .find(|project| project.slug == slug)
        .ok_or_else(|| VogtError::NotFound(format!("no project {slug:?}")))
}

fn resolve_work_item(view: &impl ReadView, reference: &str) -> Result<WorkItem, VogtError> {
    view.work_item_by_ref(reference)?
        .ok_or_else(|| VogtError::NotFound(format!("no work item {reference:?}")))
}

fn base64_encode(raw: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    for chunk in raw.chunks(3) {
        let mut buffer = [0u8; 3];
        buffer[..chunk.len()].copy_from_slice(chunk);
        let value = u32::from_be_bytes([0, buffer[0], buffer[1], buffer[2]]);
        out.push(TABLE[((value >> 18) & 0x3f) as usize] as char);
        out.push(TABLE[((value >> 12) & 0x3f) as usize] as char);
        if chunk.len() > 1 {
            out.push(TABLE[((value >> 6) & 0x3f) as usize] as char);
        }
        if chunk.len() > 2 {
            out.push(TABLE[(value & 0x3f) as usize] as char);
        }
    }
    out
}

fn base64_decode(text: &str) -> Result<Vec<u8>, ()> {
    let padded = format!("{text}{}", "=".repeat((4 - text.len() % 4) % 4));
    let mut out = Vec::new();
    let mut buffer = 0u32;
    let mut bits = 0;
    for byte in padded.bytes() {
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'-' | b'+' => 62,
            b'_' | b'/' => 63,
            b'=' => break,
            _ => return Err(()),
        };
        buffer = (buffer << 6) | u32::from(value);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buffer >> bits) as u8);
            buffer &= (1 << bits) - 1;
        }
    }
    Ok(out)
}

// The session and grant entry builders need the live engine session list joined
// with declared sessions. They stay here so the shapes are pinned, and are
// reached once the engine session join lands. Until then `collect` reports the
// agent source as unconfigured coverage rather than inventing entries.
#[allow(dead_code)]
fn session_shapes(session: &CodingSession, grant: &SessionGrant) -> (String, String) {
    (session.id.clone(), grant.id.clone())
}

#[allow(dead_code)]
const _BRANCH_KIND: &str = BRANCH_CONCLUDED_KIND;
