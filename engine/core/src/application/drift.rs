//! Drift proposals: raising them from evidence, listing them, resolving them.
//!
//! Ports `src/vogt/application/services/drift_service.py`. Detection is a
//! comparison, not a collector: it reads the declared store and the observed
//! store and writes only proposals. It refuses when no sweep has ever
//! finished, because "nothing differs" and "nothing was collected" are
//! different answers. Reconciliation only marks a proposal stale once the
//! collector that produced it has swept again since it was opened.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{json, Value};

use crate::adapters::forge::current_collector;
use crate::application::context::{write_of, AppContext, Built};
use crate::application::resolve;
use crate::application::writes::{audited_write, WriteOutcome};
use crate::core::{Clock, IdFactory, Moment, Observation};
use crate::decisions::{self, DriftFinding, EvidenceSnapshot};
use crate::errors::VogtError;
use crate::storage::interface::{
    DeclaredStore, ObservedStore, ProjectUpdate, ReadView, WorkFilter, WorkItemUpdate, WriteTxn,
};

const DRIFT_RAISED_EVENT: &str = "drift.raised";
const DRIFT_RESOLVED_EVENT: &str = "drift.resolved";
const DRIFT_SUPERSEDED_EVENT: &str = "drift.superseded";

const RESOLUTIONS: &[&str] = &["accepted", "rejected", "contested"];
const HEALTHY: &[&str] = &["active", "maintenance"];

/// The dependency-ref scopes, as the collector stores them.
const SCOPE_INTERNAL: &str = "internal";
const SCOPE_BROKEN: &str = "broken";

const KIND_TAG: &str = "git.tag";
const KIND_RELEASE: &str = "release";
const KIND_ISSUE: &str = "forge.issue";
const KIND_CI_CHECK: &str = "ci.check";
const KIND_POSTURE: &str = "forge.posture";

const COLLECTOR_ISSUES: &str = "gh-issues";

const OBSERVATION_LIMIT: i64 = 200;

/// The posture facts a project can lack, and the words the finding uses.
const POSTURE_FACTS: &[(&str, &str)] = &[
    ("version_updates", "version updates"),
    ("vulnerability_alerts", "vulnerability alerts"),
    ("automated_security_fixes", "automated security fixes"),
];

pub fn drift_detect_op(ctx: &Built, params: Value) -> Result<Value, VogtError> {
    // `auto_accept` defaults to true in the schema, which fills it before the
    // service runs. A null is left as null, and that is not an acceptance.
    let auto_accept = params.get("auto_accept").and_then(Value::as_bool) == Some(true);
    let reason = field(&params, "drift.detect", "reason")?;
    crate::with_ctx!(ctx, |ctx| detect(ctx, auto_accept, &reason))
}

pub fn drift_list_op(ctx: &Built, params: Value) -> Result<Value, VogtError> {
    // Absent means "open", the CLI default. An explicit null means no status
    // filter at all: MCP and HTTP can send one, and Python lists every status.
    let status = match params.get("status") {
        Some(Value::String(status)) => Some(status.clone()),
        // Explicit null: no status filter. The schema default of "open" only
        // fills a field the caller left out, so a sent null must stay unfiltered.
        _ => None,
    };
    let kind = params
        .get("kind")
        .and_then(Value::as_str)
        .map(str::to_string);
    let project = params
        .get("project")
        .and_then(Value::as_str)
        .map(str::to_string);
    let limit = params
        .get("limit")
        .and_then(Value::as_i64)
        .ok_or_else(|| VogtError::InvalidRequest("drift.list needs a limit".to_string()))?;
    crate::with_ctx!(ctx, |ctx| list(
        ctx,
        status.as_deref(),
        kind.as_deref(),
        project.as_deref(),
        limit
    ))
}

pub fn drift_resolve_op(ctx: &Built, params: Value) -> Result<Value, VogtError> {
    let id = field(&params, "drift.resolve", "id")?;
    let resolution = field(&params, "drift.resolve", "resolution")?;
    let reason = field(&params, "drift.resolve", "reason")?;
    crate::with_ctx!(ctx, |ctx| resolve_drift(ctx, &id, &resolution, &reason))
}

fn detect<C: Clock + 'static, I: IdFactory + 'static>(
    ctx: &AppContext<C, I>,
    auto_accept: bool,
    reason: &str,
) -> Result<Value, VogtError> {
    let reason = reason.to_string();
    if !ctx.observed.has_evidence_tables()? || ctx.observed.coverage()?.is_empty() {
        return Err(VogtError::InvalidRequest(
            "no collector has completed a sweep, so there is nothing to compare \
             declared state against — run `sweep` first"
                .to_string(),
        ));
    }
    let findings = findings_of(&ctx.declared.read()?, &ctx.observed)?;
    let found = findings.len();
    // Read once, before the loop. Python skips a finding that is already open
    // with no write at all (`drift_service.py`); checking inside the write
    // committed an empty audit row and an empty event for every one of them,
    // and re-reading per finding also collapsed two same-key findings in a
    // single run, which Python raises both of.
    let already = ctx.declared.read()?.open_drift_subjects()?;

    let mut raised = Vec::new();
    let mut auto_accepted = Vec::new();
    for finding in &findings {
        let key = (
            finding.kind.clone(),
            finding.subject_kind.clone(),
            finding.subject_id.clone(),
        );
        if already.contains(&key) {
            continue;
        }
        let proposal = proposal_for(ctx, finding)?;
        let id = proposal.id.clone();
        raise(ctx, &proposal, &reason)?;
        // The answer carries the proposal as it was raised, not as it stands
        // after auto-accept. Python appends the in-memory value before
        // resolving, so an auto-accepted proposal comes back open.
        raised.push(proposal);
        if auto_accept && decisions::auto_acceptable(&finding.kind) {
            let fixed = format!(
                "auto-accepted under the shipped low-risk policy ({} is a state-sync kind)",
                finding.kind
            );
            resolve_drift(ctx, &id, "accepted", &fixed)?;
            auto_accepted.push(id);
        }
    }
    let superseded = reconcile(ctx, &findings, &reason)?;
    let not_collected = not_collected_of(&ctx.declared.read()?, &ctx.observed)?;
    let mut kinds: Vec<&str> = decisions::AUTO_ACCEPTABLE_KINDS.to_vec();
    kinds.sort_unstable();
    Ok(json!({
        "raised": raised,
        "auto_accepted": auto_accepted,
        "already_open": found - raised.len(),
        "superseded": superseded,
        "not_collected": not_collected,
        "auto_acceptable_kinds": kinds,
    }))
}

fn list<C: Clock, I: IdFactory>(
    ctx: &AppContext<C, I>,
    status: Option<&str>,
    kind: Option<&str>,
    project: Option<&str>,
    limit: i64,
) -> Result<Value, VogtError> {
    let declared = ctx.declared.read()?;
    let project_id = match project {
        Some(slug) => Some(resolve::project(&declared, slug)?.id),
        None => None,
    };
    let proposals = declared.list_drift(status, kind, project_id.as_deref(), limit)?;
    // Insertion order, not sorted. `BTreeMap` would alphabetise the keys, and
    // Python's answer keeps `HUMAN_GATED_REASON`'s order on every list.
    let mut human_gated = serde_json::Map::new();
    for (kind, reason) in decisions::HUMAN_GATED_REASON {
        human_gated.insert((*kind).to_string(), Value::String((*reason).to_string()));
    }
    let freshness = freshness_of(&ctx.observed, clock_now(&ctx.clock))?;
    Ok(json!({
        "proposals": proposals,
        "human_gated": human_gated,
        "freshness": freshness,
    }))
}

fn resolve_drift<C: Clock + 'static, I: IdFactory + 'static>(
    ctx: &AppContext<C, I>,
    id: &str,
    resolution: &str,
    reason: &str,
) -> Result<Value, VogtError> {
    if !RESOLUTIONS.contains(&resolution) {
        return Err(VogtError::InvalidRequest(format!(
            "resolution must be one of {}",
            RESOLUTIONS.join(", ")
        )));
    }
    let id = id.to_string();
    let resolution = resolution.to_string();
    let reason = reason.to_string();
    let mut write = write_of(ctx);
    let clock = std::sync::Arc::clone(write.clock());
    audited_write(&mut write, "drift.resolve", &reason, |txn, actor| {
        let proposal = txn.drift_by_id(&id)?.ok_or_else(|| {
            VogtError::NotFound(format!("no drift proposal {}", crate::core::py_repr(&id)))
        })?;
        if proposal.status.to_string() != "open" {
            return Err(VogtError::Conflict(format!(
                "proposal {} was already {}",
                crate::core::py_repr(&id),
                proposal.status
            )));
        }
        let change_applied = if resolution == "accepted" {
            apply(txn, &proposal, clock_now(&clock))?
        } else {
            false
        };
        txn.resolve_drift(&id, &resolution, &actor.id, &reason, clock_now(&clock))?;
        let updated = txn.drift_by_id(&id)?.ok_or_else(|| {
            VogtError::NotFound(format!("no drift proposal {}", crate::core::py_repr(&id)))
        })?;
        Ok(WriteOutcome {
            result: json!({ "proposal": updated, "change_applied": change_applied }),
            entity_kind: "drift_proposal".to_string(),
            entity_id: id.clone(),
            payload: serde_json::to_value(&updated).unwrap_or(Value::Null),
            event_kind: DRIFT_RESOLVED_EVENT.to_string(),
            summary: json!({
                "kind": updated.kind,
                "resolution": resolution,
                "change_applied": change_applied,
            }),
        })
    })
}

/// Apply an accepted proposal. Only the two state-sync kinds change anything;
/// every other kind is a question a person answers and records nothing else.
fn apply(
    txn: &mut impl WriteTxn,
    proposal: &crate::core::DriftProposal,
    now: Moment,
) -> Result<bool, VogtError> {
    let change = &proposal.proposed_change;
    match proposal.kind.as_str() {
        "forge_state_mismatch" => {
            let (Some(work_ref), Some(to)) = (text(change, "work_ref"), text(change, "to")) else {
                return Ok(false);
            };
            let Some(item) = txn.work_item_by_ref(&work_ref)? else {
                return Ok(false);
            };
            txn.update_work_item(
                &item.id,
                &WorkItemUpdate {
                    state: Some(to),
                    ..WorkItemUpdate::default()
                },
                now,
            )?;
            Ok(true)
        }
        decisions::VERSION_MISMATCH => {
            if text(change, "entity").as_deref() != Some("project") {
                return Ok(false);
            }
            let (Some(project_id), Some(to)) = (proposal.project_id.clone(), text(change, "to"))
            else {
                return Ok(false);
            };
            txn.update_project(
                &project_id,
                &ProjectUpdate {
                    current_version: Some(to),
                    ..ProjectUpdate::default()
                },
                now,
            )?;
            Ok(true)
        }
        _ => Ok(false),
    }
}

/// The proposal, built before the write. Python draws the id and the opened-at
/// timestamp before `audited_write` (`drift_service.py`), and the row carries
/// no project slug — the slug is resolved only for the event summary.
fn proposal_for<C: Clock, I: IdFactory>(
    ctx: &AppContext<C, I>,
    finding: &DriftFinding,
) -> Result<crate::core::DriftProposal, VogtError> {
    let write = write_of(ctx);
    let id = write
        .ids()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .next("dft");
    let now = clock_now(write.clock());
    Ok(crate::core::DriftProposal {
        id,
        kind: finding.kind.clone(),
        subject_kind: finding.subject_kind.clone(),
        subject_id: finding.subject_id.clone(),
        project_id: finding.project_id.clone(),
        project_slug: None,
        summary: finding.summary.clone(),
        evidence_observation_id: finding.evidence_observation_id.clone(),
        evidence_snapshot: evidence_json(&finding.evidence),
        proposed_change: finding.proposed_change.clone(),
        status: crate::core::DriftStatus::Open,
        opened_at: now,
        superseded_at: None,
        superseded_detail: None,
        resolved_by_actor_id: None,
        resolved_by_identity_ref: None,
        resolved_at: None,
        resolution_reason: None,
    })
}

fn raise<C: Clock + 'static, I: IdFactory + 'static>(
    ctx: &AppContext<C, I>,
    proposal: &crate::core::DriftProposal,
    reason: &str,
) -> Result<(), VogtError> {
    let reason = reason.to_string();
    let mut write = write_of(ctx);
    audited_write(&mut write, "drift.detect", &reason, |txn, _actor| {
        txn.insert_drift(proposal)?;
        let mut summary = serde_json::Map::new();
        summary.insert("kind".to_string(), Value::String(proposal.kind.clone()));
        summary.insert(
            "summary".to_string(),
            Value::String(proposal.summary.clone()),
        );
        // Omitted, not null, when the proposal names no project.
        if let Some(project_id) = proposal.project_id.as_deref() {
            if let Some(project) = txn.project_by_id(project_id)? {
                summary.insert("project".to_string(), Value::String(project.slug));
            }
        }
        Ok(WriteOutcome {
            result: serde_json::to_value(proposal).unwrap_or(Value::Null),
            entity_kind: "drift_proposal".to_string(),
            entity_id: proposal.id.clone(),
            payload: serde_json::to_value(proposal).unwrap_or(Value::Null),
            event_kind: DRIFT_RAISED_EVENT.to_string(),
            summary: Value::Object(summary),
        })
    })?;
    Ok(())
}

/// Coverage-gated reconciliation against the findings this run already read.
/// A reappearing finding clears a stale flag; an absent one is marked only
/// once its collector has swept again since the proposal opened, and only
/// once — a proposal already flagged is left alone. Nothing is auto-resolved.
fn reconcile<C: Clock + 'static, I: IdFactory + 'static>(
    ctx: &AppContext<C, I>,
    findings: &[DriftFinding],
    reason: &str,
) -> Result<Vec<String>, VogtError> {
    let open = ctx
        .declared
        .read()?
        .list_drift(Some("open"), None, None, 10_000)?;
    let current: BTreeSet<(String, String, String)> = findings
        .iter()
        .map(|finding| {
            (
                finding.kind.clone(),
                finding.subject_kind.clone(),
                finding.subject_id.clone(),
            )
        })
        .collect();
    let coverage = ctx.observed.coverage()?;
    let mut marked = Vec::new();
    for proposal in open {
        let key = (
            proposal.kind.clone(),
            proposal.subject_kind.clone(),
            proposal.subject_id.clone(),
        );
        if current.contains(&key) {
            if proposal.superseded_at.is_some() {
                mark(ctx, &proposal, None, None, reason)?;
            }
            continue;
        }
        if proposal.superseded_at.is_some() {
            continue;
        }
        let collector = proposal
            .evidence_snapshot
            .get("collector")
            .and_then(Value::as_str)
            .unwrap_or("");
        let name = current_collector(collector);
        let Some(finished) = coverage.get(name).and_then(|sweep| sweep.finished_at) else {
            continue;
        };
        if finished > proposal.opened_at {
            let detail = format!(
                "{collector} completed a sweep at {}, after this was raised, and \
                 the condition that raised it no longer reproduces — the evidence \
                 snapshot above is what it was raised on",
                finished.to_iso()
            );
            mark(
                ctx,
                &proposal,
                Some(detail),
                Some(clock_now(&ctx.clock)),
                reason,
            )?;
            marked.push(proposal.id);
        }
    }
    Ok(marked)
}

fn mark<C: Clock + 'static, I: IdFactory + 'static>(
    ctx: &AppContext<C, I>,
    proposal: &crate::core::DriftProposal,
    detail: Option<String>,
    at: Option<Moment>,
    reason: &str,
) -> Result<(), VogtError> {
    let id = proposal.id.clone();
    let kind = proposal.kind.clone();
    let reason = reason.to_string();
    let mut write = write_of(ctx);
    audited_write(&mut write, "drift.detect", &reason, |txn, _actor| {
        txn.mark_drift_superseded(&id, detail.as_deref(), at)?;
        let updated = txn.drift_by_id(&id)?;
        Ok(WriteOutcome {
            result: Value::Null,
            entity_kind: "drift_proposal".to_string(),
            entity_id: id.clone(),
            payload: updated
                .as_ref()
                .map(|proposal| serde_json::to_value(proposal).unwrap_or(Value::Null))
                .unwrap_or(Value::Null),
            event_kind: DRIFT_SUPERSEDED_EVENT.to_string(),
            summary: json!({
                "kind": kind,
                "superseded": at.is_some(),
                "detail": detail,
            }),
        })
    })?;
    Ok(())
}

fn findings_of(
    declared: &impl ReadView,
    observed: &impl ObservedStore,
) -> Result<Vec<DriftFinding>, VogtError> {
    let mut findings = Vec::new();
    findings.extend(version_findings(declared, observed)?);
    findings.extend(dependency_findings(declared, observed)?);
    findings.extend(forge_findings(declared, observed)?);
    findings.extend(initiative_findings(declared, observed)?);
    Ok(findings)
}

fn version_findings(
    declared: &impl ReadView,
    observed: &impl ObservedStore,
) -> Result<Vec<DriftFinding>, VogtError> {
    let mut out = Vec::new();
    for project in declared.list_projects(1000, 0)? {
        let observations = observed.latest(
            &[KIND_TAG.to_string(), KIND_RELEASE.to_string()],
            Some(&project.id),
            false,
            false,
            100,
        )?;
        // The greatest tag string, not the newest observation. An empty tag is
        // skipped rather than winning, and a project whose tags are all empty
        // raises nothing.
        let best = observations
            .iter()
            .filter(|observation| {
                observation
                    .payload
                    .get("tag")
                    .and_then(Value::as_str)
                    .is_some_and(|tag| !tag.is_empty())
            })
            .max_by_key(|observation| {
                observation
                    .payload
                    .get("tag")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string()
            });
        let Some(observation) = best else {
            continue;
        };
        let tag = observation.payload["tag"].as_str().unwrap_or("");
        if let Some(finding) = decisions::version_mismatch(
            &project.id,
            &project.slug,
            project.current_version.as_deref(),
            tag,
            snapshot(observation),
            Some(&observation.id),
        ) {
            out.push(finding);
        }
    }
    Ok(out)
}

fn dependency_findings(
    declared: &impl ReadView,
    observed: &impl ObservedStore,
) -> Result<Vec<DriftFinding>, VogtError> {
    let mut out = Vec::new();
    let slugs: BTreeMap<String, String> = declared
        .list_projects(1000, 0)?
        .into_iter()
        .map(|project| (project.id, project.slug))
        .collect();
    for dep in observed.dep_refs(None, None)? {
        if dep.to_project_id.is_some() {
            continue;
        }
        let Some(observation) = observed.latest_by_subject(&dep.subject_key)? else {
            continue;
        };
        let scope = observation
            .payload
            .get("scope")
            .and_then(Value::as_str)
            .unwrap_or("external");
        if scope == SCOPE_INTERNAL {
            continue;
        }
        let slug = slugs
            .get(&dep.from_project_id)
            .map(String::as_str)
            .unwrap_or(dep.from_project_id.as_str());
        let manifest = dep.manifest.as_deref();
        let finding = if scope == SCOPE_BROKEN {
            decisions::broken_path_dependency(
                &dep.subject_key,
                &dep.from_project_id,
                slug,
                &dep.raw_target,
                manifest,
                snapshot(&observation),
                Some(&observation.id),
            )
        } else {
            decisions::unresolved_dependency(
                &dep.subject_key,
                &dep.from_project_id,
                slug,
                &dep.raw_target,
                manifest,
                snapshot(&observation),
                Some(&observation.id),
            )
        };
        out.push(finding);
    }
    Ok(out)
}

fn forge_findings(
    declared: &impl ReadView,
    observed: &impl ObservedStore,
) -> Result<Vec<DriftFinding>, VogtError> {
    let mut out = Vec::new();
    let coverage = observed.coverage()?;
    let items = declared.list_work_items(&WorkFilter {
        limit: 1000,
        ..WorkFilter::default()
    })?;
    for item in &items {
        let links = declared.work_links_for_subjects_by_item(&item.id)?;
        for subject_key in links.keys() {
            if !subject_key.starts_with("gh:") {
                continue;
            }
            let Some(observation) = observed.latest_by_subject(subject_key)? else {
                let swept = coverage
                    .get(current_collector(COLLECTOR_ISSUES))
                    .and_then(|sweep| sweep.finished_at);
                if let Some(swept_at) = swept {
                    out.push(decisions::vanished_upstream(
                        &item.id,
                        &item.reference,
                        subject_key,
                        item.project_id.as_deref(),
                        swept_at,
                    ));
                }
                continue;
            };
            let upstream = observation
                .payload
                .get("state")
                .and_then(Value::as_str)
                .unwrap_or("open");
            let terminal = crate::core::TERMINAL_STATES.contains(&item.state.as_str());
            if (upstream == "closed") != terminal {
                out.push(decisions::forge_state_mismatch(
                    &item.id,
                    &item.reference,
                    &item.state,
                    upstream,
                    subject_key,
                    item.project_id.as_deref(),
                    snapshot(&observation),
                    Some(&observation.id),
                ));
            }
        }
        out.extend(referenced_issue_findings(observed, item, &links)?);
    }
    out.extend(ci_findings(declared, observed)?);
    out.extend(posture_findings(declared, observed)?);
    Ok(out)
}

fn referenced_issue_findings(
    observed: &impl ObservedStore,
    item: &crate::core::WorkItem,
    links: &BTreeMap<String, String>,
) -> Result<Vec<DriftFinding>, VogtError> {
    let mut out = Vec::new();
    let linked: BTreeSet<&str> = links.keys().map(String::as_str).collect();
    let text = format!("{}\n{}", item.title, item.body);
    for reference in decisions::issue_references(&text) {
        if linked.contains(reference.as_str()) {
            continue;
        }
        let Some(observation) = observed.latest_by_subject(&reference)? else {
            continue;
        };
        if observation.kind != KIND_ISSUE {
            continue;
        }
        let upstream = observation
            .payload
            .get("state")
            .and_then(Value::as_str)
            .unwrap_or("open");
        let terminal = crate::core::TERMINAL_STATES.contains(&item.state.as_str());
        if (upstream == "closed") != terminal {
            out.push(decisions::referenced_issue_state_mismatch(
                &item.id,
                &item.reference,
                &item.state,
                upstream,
                &reference,
                item.project_id.as_deref(),
                snapshot(&observation),
                Some(&observation.id),
            ));
        }
    }
    Ok(out)
}

fn ci_findings(
    declared: &impl ReadView,
    observed: &impl ObservedStore,
) -> Result<Vec<DriftFinding>, VogtError> {
    let mut out = Vec::new();
    for project in declared.list_projects(1000, 0)? {
        let checks = observed.latest(
            &[KIND_CI_CHECK.to_string()],
            Some(&project.id),
            false,
            false,
            OBSERVATION_LIMIT,
        )?;
        let Some(rollup) = decisions::roll_up(&checks) else {
            continue;
        };
        if rollup.failing.is_empty()
            || !HEALTHY.contains(&project.lifecycle_state.to_string().as_str())
        {
            continue;
        }
        let newest = rollup
            .checks
            .iter()
            .filter(|check| failing_check(check))
            .max_by_key(|check| check.observed_at);
        let Some(newest) = newest else {
            continue;
        };
        let failing: Vec<&str> = rollup.failing.iter().map(String::as_str).collect();
        out.push(decisions::ci_red_vs_healthy(
            &project.id,
            &project.slug,
            &project.lifecycle_state.to_string(),
            &failing,
            &rollup.revision,
            snapshot(newest),
            Some(&newest.id),
        ));
    }
    Ok(out)
}

fn failing_check(check: &Observation) -> bool {
    let conclusion = check.payload.get("conclusion").and_then(Value::as_str);
    !matches!(conclusion, None | Some("success") | Some("skipped"))
}

fn posture_findings(
    declared: &impl ReadView,
    observed: &impl ObservedStore,
) -> Result<Vec<DriftFinding>, VogtError> {
    let mut out = Vec::new();
    for project in declared.list_projects(1000, 0)? {
        let observations = observed.latest(
            &[KIND_POSTURE.to_string()],
            Some(&project.id),
            false,
            false,
            1,
        )?;
        let Some(observation) = observations.first() else {
            continue;
        };
        let missing: Vec<&str> = POSTURE_FACTS
            .iter()
            .filter(|(key, _)| observation.payload.get(*key) == Some(&Value::Bool(false)))
            .map(|(_, words)| *words)
            .collect();
        if missing.is_empty() {
            continue;
        }
        out.push(decisions::update_automation_gap(
            &project.id,
            &project.slug,
            &missing,
            snapshot(observation),
            Some(&observation.id),
        ));
    }
    Ok(out)
}

/// The initiative tracking-issue check. `upstream.linked_projects` and
/// `upstream.upstream_items` are not ported as a module, so the join they do
/// is inlined: on a linked project the members are the observed `forge.issue`
/// rows joined to the overlay that carries the initiative id and the state.
/// The comparison itself is the tracking issue's checkboxes against those
/// member states.
fn initiative_findings(
    declared: &impl ReadView,
    observed: &impl ObservedStore,
) -> Result<Vec<DriftFinding>, VogtError> {
    let initiatives = declared.list_initiatives(1000, 0)?;
    if initiatives.is_empty() {
        return Ok(Vec::new());
    }
    let projects = declared.list_projects(1000, 0)?;
    let mut expected: BTreeMap<(String, String), BTreeMap<i64, bool>> = BTreeMap::new();
    let mut member_refs: BTreeMap<(String, String, i64), String> = BTreeMap::new();
    for project in projects
        .iter()
        .filter(|project| project.link_state.to_string() == "linked")
    {
        let observations = observed.latest(
            &[KIND_ISSUE.to_string()],
            Some(&project.id),
            false,
            false,
            OBSERVATION_LIMIT,
        )?;
        let keys: Vec<String> = observations
            .iter()
            .map(|observation| observation.subject_key.clone())
            .collect();
        let adopted = declared.work_links_for_subjects(&keys)?;
        let overlays = declared.work_overlays(&keys)?;
        for observation in &observations {
            if adopted.contains_key(&observation.subject_key) {
                continue;
            }
            let Some(overlay) = overlays.get(&observation.subject_key) else {
                continue;
            };
            let (Some(initiative_id), Some(number)) = (
                overlay.initiative_id.clone(),
                forge_number(&observation.subject_key),
            ) else {
                continue;
            };
            let state = overlay.workflow_state.as_deref().unwrap_or("open");
            expected
                .entry((initiative_id.clone(), project.id.clone()))
                .or_default()
                .insert(number, crate::core::TERMINAL_STATES.contains(&state));
            member_refs.insert(
                (initiative_id, project.id.clone(), number),
                observation.subject_key.clone(),
            );
        }
    }
    let mut out = Vec::new();
    for observation in observed.latest(&[KIND_ISSUE.to_string()], None, false, false, 1000)? {
        let Some(body) = observation.payload.get("body").and_then(Value::as_str) else {
            continue;
        };
        if body.is_empty() {
            continue;
        }
        let Some(project_id) = observation.project_id.as_deref() else {
            continue;
        };
        for initiative in &initiatives {
            if !decisions::body_has_marker(Some(body), &initiative.slug) {
                continue;
            }
            let Some(member_state) = expected.get(&(initiative.id.clone(), project_id.to_string()))
            else {
                continue;
            };
            for (number, checked) in decisions::parse_checkbox_states(body) {
                let Ok(number) = number.parse::<i64>() else {
                    continue;
                };
                let Some(should_be) = member_state.get(&number) else {
                    continue;
                };
                if checked == *should_be {
                    continue;
                }
                let Some(work_ref) =
                    member_refs.get(&(initiative.id.clone(), project_id.to_string(), number))
                else {
                    continue;
                };
                out.push(decisions::initiative_checkbox_drift(
                    &initiative.id,
                    &initiative.slug,
                    Some(project_id),
                    &observation.subject_key,
                    number,
                    work_ref,
                    checked,
                    *should_be,
                    snapshot(&observation),
                    Some(&observation.id),
                ));
            }
        }
    }
    Ok(out)
}

fn forge_number(subject_key: &str) -> Option<i64> {
    subject_key
        .rsplit_once('#')
        .and_then(|(_, digits)| digits.parse().ok())
}

fn not_collected_of(
    declared: &impl ReadView,
    observed: &impl ObservedStore,
) -> Result<Vec<String>, VogtError> {
    if !observed.has_evidence_tables()? {
        return Ok(Vec::new());
    }
    let swept = observed.coverage_by_project()?;
    // `{collector: {project_id: moment}}`. A project counts as swept when it
    // appears under any collector, so the values are what matter.
    let mut seen = BTreeSet::new();
    for projects in swept.values() {
        seen.extend(projects.keys().cloned());
    }
    let mut slugs = Vec::new();
    for project in declared.list_projects(1000, 0)? {
        if !seen.contains(&project.id) {
            slugs.push(project.slug);
        }
    }
    slugs.sort();
    Ok(slugs)
}

fn snapshot(observation: &Observation) -> EvidenceSnapshot {
    EvidenceSnapshot {
        subject_key: observation.subject_key.clone(),
        content_digest: observation.content_digest.clone(),
        observed_at: observation.observed_at,
        collector: observation.collector.clone(),
        payload: observation.payload.clone(),
    }
}

/// `EvidenceSnapshot` does not derive serde, and the stored form is the same
/// shape Python's model dumps: the five fields, with an empty object when the
/// finding carries no evidence.
fn evidence_json(evidence: &Option<EvidenceSnapshot>) -> Value {
    let Some(evidence) = evidence else {
        return json!({});
    };
    json!({
        "subject_key": evidence.subject_key,
        "content_digest": evidence.content_digest,
        "observed_at": evidence.observed_at.to_iso(),
        "collector": evidence.collector,
        "payload": evidence.payload,
    })
}

/// `views.freshness_of`. The helper in the notifications module is
/// `pub(super)`, so the small shape is repeated here rather than widening it.
fn freshness_of(observed: &impl ObservedStore, now: Moment) -> Result<Value, VogtError> {
    if !observed.has_evidence_tables()? {
        return Ok(json!({
            "status": "never_swept",
            "oldest_relevant_sweep": Value::Null,
            "age_seconds": Value::Null,
            "collectors": {},
            "detail": "no sweep has run; observed subjects are not collected",
        }));
    }
    let newest = observed.coverage()?;
    if newest.is_empty() {
        return Ok(json!({
            "status": "never_swept",
            "oldest_relevant_sweep": Value::Null,
            "age_seconds": Value::Null,
            "collectors": {},
            "detail": "no collector has completed a sweep yet",
        }));
    }
    let mut collectors = serde_json::Map::new();
    let mut oldest: Option<Moment> = None;
    let mut partial = false;
    for (name, sweep) in &newest {
        let finished = sweep.finished_at.unwrap_or(sweep.started_at);
        collectors.insert(
            name.clone(),
            Value::String(format!(
                "{}s ago ({})",
                now.seconds_since(finished) as i64,
                sweep.outcome
            )),
        );
        if oldest.is_none_or(|held| finished < held) {
            oldest = Some(finished);
        }
        if sweep.outcome.to_string() != "ok" {
            partial = true;
        }
    }
    Ok(json!({
        "status": if partial { "partial" } else { "fresh" },
        "oldest_relevant_sweep": oldest.map(|moment| moment.to_json()),
        "age_seconds": oldest.map(|moment| now.seconds_since(moment) as i64),
        "collectors": collectors,
        "detail": if partial {
            Some("at least one collector reported a partial or failed sweep")
        } else {
            None
        },
    }))
}

fn field(params: &Value, operation: &str, name: &str) -> Result<String, VogtError> {
    params
        .get(name)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| VogtError::InvalidRequest(format!("{operation} needs a {name}")))
}

fn text(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_string)
}

fn clock_now<C: Clock>(clock: &std::sync::Arc<std::sync::Mutex<C>>) -> Moment {
    clock
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .now()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{ActorKind, Principal, SequentialIds, StepClock};

    fn context() -> Built {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "vogt-drift-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let config = crate::config::VogtConfig {
            data_dir: dir,
            ..crate::config::VogtConfig::default()
        };
        let principal = Principal::new("local:test-user", ActorKind::Human, "Test").unwrap();
        let built = crate::application::context::build_context(
            config,
            Some(principal.clone()),
            Some(StepClock::new(Moment::from_unix(1_700_000_000, 0))),
            Some(SequentialIds::new(None).unwrap()),
            None,
            None,
            None,
            None,
        )
        .unwrap();
        let Built::StepSequential(ctx) = &built else {
            unreachable!()
        };
        ctx.declared.migrate().unwrap();
        ctx.declared.bootstrap(&principal).unwrap();
        built
    }

    #[test]
    fn detect_refuses_when_nothing_has_been_swept() {
        let ctx = context();
        let error = drift_detect_op(&ctx, json!({"reason": "parity"})).unwrap_err();
        assert!(
            matches!(error, VogtError::InvalidRequest(ref message) if message.contains("run `sweep` first")),
            "{error}"
        );
    }

    #[test]
    fn human_gated_keeps_the_declared_order() {
        let ctx = context();
        let listed = drift_list_op(&ctx, json!({"limit": 100})).unwrap();
        let keys: Vec<&str> = listed["human_gated"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        let expected: Vec<&str> = decisions::HUMAN_GATED_REASON
            .iter()
            .map(|(kind, _)| *kind)
            .collect();
        assert_eq!(keys, expected);
        // The sorted order would put `broken_path_dependency` first.
        assert_ne!(keys.first().copied(), Some("broken_path_dependency"));
    }

    #[test]
    fn a_resolution_outside_the_three_is_refused_without_quotes() {
        let ctx = context();
        let error = drift_resolve_op(
            &ctx,
            json!({"id": "dft_0001", "resolution": "maybe", "reason": "parity"}),
        )
        .unwrap_err();
        let VogtError::InvalidRequest(message) = error else {
            panic!("{error}")
        };
        assert_eq!(
            message,
            "resolution must be one of accepted, rejected, contested"
        );
    }
}
