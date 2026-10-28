//! A session's own typed report of where it is (WI-2039). Ports `report` from
//! `src/vogt/application/services/reports.py`.
//!
//! The report is stored once and read three ways: the session's row, the Inbox,
//! and a note on the work item. A session reports only for itself. Free text
//! is redacted before it is stored; the SHA is a commit id and is left as given.

use serde_json::{json, Value};

use crate::application::context::{write_of, AppContext, Built};
use crate::application::resolve;
use crate::application::writes::{audited_write, validate_reason, WriteOutcome};
use crate::core::{
    py_repr, ActorKind, Clock, CodingSession, Comment, IdFactory, SessionReport,
    SessionReportFields,
};
use crate::errors::VogtError;
use crate::observability::redact;
use crate::storage::interface::{DeclaredStore, ReadView, WriteTxn};

const SESSION_REPORT: &str = "session.report";
const SESSION_REPORTED_EVENT: &str = "session.reported";

pub fn report_op(ctx: &Built, params: Value) -> Result<Value, VogtError> {
    let reason = params
        .get("reason")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let kind = params
        .get("kind")
        .and_then(Value::as_str)
        .ok_or_else(|| VogtError::InvalidRequest(format!("{SESSION_REPORT} needs a kind")))?
        .to_string();
    if !matches!(
        kind.as_str(),
        "progress" | "result" | "question" | "blocker"
    ) {
        return Err(VogtError::InvalidRequest(format!(
            "kind must be progress, result, question or blocker, not {}",
            py_repr(&kind)
        )));
    }
    let summary_raw = params
        .get("summary")
        .and_then(Value::as_str)
        .ok_or_else(|| VogtError::InvalidRequest(format!("{SESSION_REPORT} needs a summary")))?;
    let id = params.get("id").and_then(Value::as_str).map(str::to_string);
    let work_item = params
        .get("work_item")
        .and_then(Value::as_str)
        .map(str::to_string);
    let branch = optional_str(&params, "branch");
    let sha = optional_str(&params, "sha");
    let pr = optional_str(&params, "pr");
    let next_step = optional_str(&params, "next_step");
    let checks = string_list(&params, "checks");
    let blockers = string_list(&params, "blockers");
    crate::with_ctx!(ctx, |ctx| report(
        ctx,
        &reason,
        &kind,
        summary_raw,
        id.as_deref(),
        work_item.as_deref(),
        branch.as_deref(),
        sha.as_deref(),
        pr.as_deref(),
        next_step.as_deref(),
        &checks,
        &blockers,
    ))
}

#[allow(clippy::too_many_arguments)]
fn report<C: Clock + 'static, I: IdFactory + 'static>(
    ctx: &AppContext<C, I>,
    reason: &str,
    kind: &str,
    summary_raw: &str,
    id: Option<&str>,
    work_item: Option<&str>,
    branch: Option<&str>,
    sha: Option<&str>,
    pr: Option<&str>,
    next_step: Option<&str>,
    checks: &[String],
    blockers: &[String],
) -> Result<Value, VogtError> {
    let reason = validate_reason(reason)?;
    let summary = redact(summary_raw.trim());
    if summary.is_empty() {
        return Err(VogtError::InvalidRequest(
            "a report has a summary".to_string(),
        ));
    }
    let own = own_engine_session(ctx)?;
    let engine_id = reporting_session(ctx, id, own.as_deref())?;
    let fields = SessionReportFields {
        branch: blank(branch, true),
        sha: blank(sha, false),
        pr: blank(pr, true),
        checks: checks
            .iter()
            .map(|item| item.trim())
            .filter(|item| !item.is_empty())
            .map(redact)
            .collect(),
        blockers: blockers
            .iter()
            .map(|item| item.trim())
            .filter(|item| !item.is_empty())
            .map(redact)
            .collect(),
        next_step: blank(next_step, true),
    };
    let mut writing = write_of(ctx);
    // Python reads the clock and mints ids inside the write, after the
    // work-item lookup. The factory is shared, so minting here (before the
    // actor row) only differs when a new actor is created in the same call,
    // which the parity fixtures do not exercise for this operation.
    let now = writing
        .clock()
        .lock()
        .expect("the shared clock is not poisoned")
        .now();
    let report_id = writing
        .ids()
        .lock()
        .expect("the shared ids are not poisoned")
        .next("rpt");
    let comment_id = writing
        .ids()
        .lock()
        .expect("the shared ids are not poisoned")
        .next("cmt");
    let view = audited_write(&mut writing, SESSION_REPORT, &reason, |txn, actor| {
        let work_item_id = work_item_id(txn, work_item, &engine_id)?;
        let row = SessionReport {
            id: report_id.clone(),
            actor_id: actor.id.clone(),
            engine_session_id: Some(engine_id.clone()),
            work_item_id: work_item_id.clone(),
            kind: kind.to_string(),
            summary: summary.clone(),
            fields: fields.clone(),
            created_at: now,
        };
        txn.insert_session_report(&row)?;
        if work_item_id.is_some() {
            txn.insert_comment(&Comment {
                id: comment_id.clone(),
                work_item_id: work_item_id.clone().unwrap_or_default(),
                actor_id: actor.id.clone(),
                actor_display_name: actor.display_name.clone(),
                body: comment_body(&row),
                created_at: now,
                kind: "note".to_string(),
                handoff: None,
            })?;
        }
        let result = view_of(txn, &row)?;
        let work_item_ref = result
            .get("work_item")
            .and_then(Value::as_str)
            .map(str::to_string);
        let payload = report_payload(&row);
        let clipped: String = summary.chars().take(200).collect();
        Ok(WriteOutcome {
            result,
            entity_kind: "session_report".to_string(),
            entity_id: row.id.clone(),
            payload,
            event_kind: SESSION_REPORTED_EVENT.to_string(),
            summary: json!({
                "report_id": row.id,
                "kind": row.kind,
                "engine_session_id": engine_id,
                "work_item": work_item_ref,
                "summary": clipped,
            }),
        })
    })?;
    Ok(json!({ "report": view }))
}

/// `SessionReport.model_dump(mode="json")`: declaration order, nulls present.
fn report_payload(row: &SessionReport) -> Value {
    json!({
        "id": row.id,
        "actor_id": row.actor_id,
        "engine_session_id": row.engine_session_id,
        "work_item_id": row.work_item_id,
        "kind": row.kind,
        "summary": row.summary,
        "fields": {
            "branch": row.fields.branch,
            "sha": row.fields.sha,
            "pr": row.fields.pr,
            "checks": row.fields.checks,
            "blockers": row.fields.blockers,
            "next_step": row.fields.next_step,
        },
        "created_at": row.created_at.to_json(),
    })
}

/// `SessionReportView`, declaration order.
fn view_of(view: &impl ReadView, row: &SessionReport) -> Result<Value, VogtError> {
    let item = match &row.work_item_id {
        Some(id) => view.work_item_by_id(id)?,
        None => None,
    };
    let actor = view.actor_by_id(&row.actor_id)?;
    Ok(json!({
        "id": row.id,
        "kind": row.kind,
        "summary": row.summary,
        "session_id": row.engine_session_id,
        "work_item": item.map(|item| item.reference.clone()),
        "branch": row.fields.branch,
        "sha": row.fields.sha,
        "pr": row.fields.pr,
        "checks": row.fields.checks,
        "blockers": row.fields.blockers,
        "next_step": row.fields.next_step,
        "reporter": actor.map(|actor| actor.identity_ref),
        "created_at": row.created_at.to_json(),
    }))
}

pub(crate) fn comment_body(row: &SessionReport) -> String {
    let mut lines = vec![format!("Session report ({}): {}", row.kind, row.summary)];
    let facts: Vec<String> = [
        ("branch", row.fields.branch.as_deref()),
        ("sha", row.fields.sha.as_deref()),
        ("pr", row.fields.pr.as_deref()),
    ]
    .into_iter()
    .filter_map(|(name, value)| value.map(|value| format!("{name}: {value}")))
    .collect();
    if !facts.is_empty() {
        lines.push(facts.join(" · "));
    }
    if !row.fields.checks.is_empty() {
        lines.push(format!("checks: {}", row.fields.checks.join("; ")));
    }
    if !row.fields.blockers.is_empty() {
        lines.push(format!("blockers: {}", row.fields.blockers.join("; ")));
    }
    if let Some(next) = &row.fields.next_step {
        lines.push(format!("next: {next}"));
    }
    if let Some(engine) = &row.engine_session_id {
        if !engine.is_empty() {
            lines.push(format!("session: {engine}"));
        }
    }
    lines.join("\n")
}

fn own_engine_session<C: Clock + 'static, I: IdFactory + 'static>(
    ctx: &AppContext<C, I>,
) -> Result<Option<String>, VogtError> {
    let reference = ctx.principal.identity_ref.as_str();
    if let Some(rest) = reference.strip_prefix("agent:engine:") {
        return Ok(Some(rest.to_string()));
    }
    if let Some(session_id) = reference.strip_prefix("agent:session:") {
        let session = ctx.declared.read()?.session_by_id(session_id)?;
        return Ok(session.map(|session| session.engine_session_id));
    }
    Ok(None)
}

fn reporting_session<C: Clock + 'static, I: IdFactory + 'static>(
    ctx: &AppContext<C, I>,
    wanted: Option<&str>,
    own: Option<&str>,
) -> Result<String, VogtError> {
    let Some(wanted) = wanted.map(str::trim).filter(|value| !value.is_empty()) else {
        return own.map(str::to_string).ok_or_else(|| {
            VogtError::InvalidRequest(
                "give `id`: this caller is not a session Vogt started, so it does not name one"
                    .to_string(),
            )
        });
    };
    let mut target = wanted.to_string();
    if let Some(session_id) = target.strip_prefix("ses_").map(|_| target.clone()) {
        let session = ctx.declared.read()?.session_by_id(&session_id)?;
        let Some(session) = session else {
            return Err(VogtError::InvalidRequest(format!(
                "no session {}",
                py_repr(&session_id)
            )));
        };
        target = session.engine_session_id;
    }
    if let Some(own) = own {
        if target != own {
            return Err(VogtError::ReportRefused(
                "a session reports only for itself".to_string(),
            ));
        }
    } else if ctx.principal.kind == ActorKind::Agent {
        return Err(VogtError::ReportRefused(
            "an agent that is not a session cannot report for one".to_string(),
        ));
    }
    Ok(target)
}

fn work_item_id(
    txn: &impl ReadView,
    reference: Option<&str>,
    engine_id: &str,
) -> Result<Option<String>, VogtError> {
    if let Some(reference) = reference.map(str::trim).filter(|value| !value.is_empty()) {
        return Ok(Some(resolve::work_item(txn, reference)?.id));
    }
    let session = txn.session_by_engine_id(engine_id)?;
    Ok(session.and_then(|session: CodingSession| session.work_item_id))
}

fn blank(value: Option<&str>, scrub: bool) -> Option<String> {
    let cleaned = value?.trim();
    if cleaned.is_empty() {
        return None;
    }
    Some(if scrub {
        redact(cleaned)
    } else {
        cleaned.to_string()
    })
}

fn optional_str(params: &Value, key: &str) -> Option<String> {
    params.get(key).and_then(Value::as_str).map(str::to_string)
}

fn string_list(params: &Value, key: &str) -> Vec<String> {
    params
        .get(key)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::application::context::build_context;
    use crate::application::writes::audited_write;
    use crate::core::{
        ActorKind, CodingSession, Moment, Origin, Principal, Priority, StepClock, TrustState,
        WorkItem, WorkKind,
    };
    use crate::errors::VogtError;
    use crate::storage::interface::{DeclaredStore, ReadView, WriteTxn};

    fn fresh(name: &str, principal: Principal) -> crate::application::context::Built {
        let dir = std::env::temp_dir().join(format!("vogt-report-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut clock = None;
        let mut ids = None;
        crate::application::instance::init(&dir, &mut clock, &mut ids).unwrap();
        build_context(
            crate::config::VogtConfig {
                data_dir: dir,
                ..crate::config::VogtConfig::default()
            },
            Some(principal),
            Some(StepClock::new(Moment::from_unix(1_700_000_000, 0))),
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap()
    }

    fn moment() -> Moment {
        Moment::from_unix(1_700_000_000, 0)
    }

    fn seed(built: &crate::application::context::Built) {
        crate::with_ctx!(built, |ctx| {
            let mut write = crate::application::context::write_of(ctx);
            audited_write(&mut write, "seed", "test", |txn, actor| {
                txn.insert_project(&crate::core::Project::new(
                    "prj_1",
                    "vogt",
                    "Vogt",
                    "/work",
                    moment(),
                ))?;
                txn.insert_work_item(&WorkItem {
                    id: "wrk_1".into(),
                    reference: "WI-1".into(),
                    kind: WorkKind::Feature,
                    title: "Reports".into(),
                    body: String::new(),
                    state: "open".into(),
                    priority: Priority::P2,
                    effort: None,
                    project_id: Some("prj_1".into()),
                    project_slug: Some("vogt".into()),
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
                txn.insert_session(&CodingSession {
                    id: "ses_0001".into(),
                    engine_session_id: "eng-1".into(),
                    project_id: "prj_1".into(),
                    work_item_id: Some("wrk_1".into()),
                    actor_id: actor.id.clone(),
                    cwd: "/work".into(),
                    template: None,
                    model: None,
                    effort: None,
                    reason: "test".into(),
                    purpose: None,
                    started_by_session: None,
                    started_by_kind: None,
                    started_at: moment(),
                    stopped_at: None,
                })?;
                txn.insert_session(&CodingSession {
                    id: "ses_0002".into(),
                    engine_session_id: "eng-2".into(),
                    project_id: "prj_1".into(),
                    work_item_id: None,
                    actor_id: actor.id.clone(),
                    cwd: "/work".into(),
                    template: None,
                    model: None,
                    effort: None,
                    reason: "test".into(),
                    purpose: None,
                    started_by_session: None,
                    started_by_kind: None,
                    started_at: moment(),
                    stopped_at: None,
                })?;
                Ok(crate::application::writes::WriteOutcome::new(
                    json!({}),
                    "session",
                    "ses_0001",
                    json!({}),
                    "session.started",
                    json!({}),
                ))
            })
        })
        .unwrap();
    }

    fn params() -> serde_json::Value {
        json!({
            "kind": "result",
            "summary": "pushed wi-1-reports, gates green",
            "branch": "wi-1-reports",
            "sha": "0123456789abcdef0123",
            "pr": "#163",
            "checks": ["pytest: 12 passed", "ruff: clean"],
            "next_step": "review",
            "reason": "report test",
        })
    }

    #[test]
    fn a_session_reports_for_itself_onto_its_bound_item() {
        let principal = Principal::new("agent:session:ses_0001", ActorKind::Agent, "w").unwrap();
        let built = fresh("self", principal);
        seed(&built);
        let reported = super::report_op(&built, params()).unwrap();
        let report = &reported["report"];
        assert_eq!(report["kind"], json!("result"));
        assert_eq!(report["session_id"], json!("eng-1"));
        assert_eq!(report["work_item"], json!("WI-1"));
        assert_eq!(report["pr"], json!("#163"));
        assert_eq!(report["sha"], json!("0123456789abcdef0123"));
        assert_eq!(
            report["checks"],
            json!(["pytest: 12 passed", "ruff: clean"])
        );
        let bodies = crate::with_ctx!(built, |ctx| {
            let view = ctx.declared.read().unwrap();
            view.comments_for("wrk_1", 20, true)
                .unwrap()
                .into_iter()
                .map(|comment| comment.body)
                .collect::<Vec<_>>()
        });
        assert!(
            bodies.iter().any(|body| {
                body.starts_with("Session report (result): pushed wi-1-reports")
                    && body.contains("pr: #163")
                    && body.contains("session: eng-1")
            }),
            "{bodies:?}"
        );
    }

    #[test]
    fn a_session_may_not_report_as_another() {
        let principal = Principal::new("agent:session:ses_0001", ActorKind::Agent, "w").unwrap();
        let built = fresh("other", principal);
        seed(&built);
        let mut body = params();
        body["id"] = json!("eng-2");
        let error = super::report_op(&built, body).unwrap_err();
        assert!(
            matches!(error, VogtError::ReportRefused(ref message) if message == "a session reports only for itself"),
            "{error}"
        );
    }

    #[test]
    fn a_caller_with_no_session_must_name_one() {
        let principal = Principal::new("local:test-user", ActorKind::Human, "Test").unwrap();
        let built = fresh("none", principal);
        seed(&built);
        let error = super::report_op(&built, params()).unwrap_err();
        assert!(
            matches!(error, VogtError::InvalidRequest(ref message) if message.contains("give `id`")),
            "{error}"
        );
    }

    #[test]
    fn credentials_in_a_report_are_redacted_and_the_sha_is_not() {
        let principal = Principal::new("agent:session:ses_0001", ActorKind::Agent, "w").unwrap();
        let built = fresh("redact", principal);
        seed(&built);
        let token = format!("ghp_{}", "A1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6Q7r8");
        let mut body = params();
        body["summary"] = json!(format!("pushed with {token}"));
        body["blockers"] = json!([format!("GITHUB_TOKEN={token} was denied")]);
        let reported = super::report_op(&built, body).unwrap();
        let stored = reported.to_string();
        assert!(!stored.contains(&token), "{stored}");
        assert!(stored.contains("[redacted]"));
        assert_eq!(reported["report"]["sha"], json!("0123456789abcdef0123"));
        let bodies = crate::with_ctx!(built, |ctx| {
            ctx.declared
                .read()
                .unwrap()
                .comments_for("wrk_1", 20, true)
                .unwrap()
                .into_iter()
                .map(|comment| comment.body)
                .collect::<Vec<_>>()
        });
        assert!(bodies.iter().all(|body| !body.contains(&token)));
    }
}
