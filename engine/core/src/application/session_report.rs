use serde_json::{json, Value};

use crate::application::context::{write_of, AppContext, Built};
use crate::application::resolve;
use crate::application::services::{next_id, now_of};
use crate::application::writes::{audited_write, validate_reason, WriteOutcome};
use crate::core::{ActorKind, Clock, Comment, IdFactory, SessionReport, SessionReportFields};
use crate::errors::VogtError;
use crate::observability::redact;
use crate::storage::interface::{DeclaredStore, ReadView, WriteTxn};

const OPERATION: &str = "session.report";
const EVENT: &str = "session.reported";

pub fn report_op(ctx: &Built, params: Value) -> Result<Value, VogtError> {
    let reason = validate_reason(params.get("reason").and_then(Value::as_str).unwrap_or(""))?;
    let kind = params.get("kind").and_then(Value::as_str).unwrap_or("");
    if !matches!(kind, "progress" | "result" | "question" | "blocker") {
        return Err(VogtError::InvalidRequest(
            "a report has a valid kind".to_string(),
        ));
    }
    let summary = redact(
        params
            .get("summary")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim(),
    );
    if summary.is_empty() {
        return Err(VogtError::InvalidRequest(
            "a report has a summary".to_string(),
        ));
    }
    crate::with_ctx!(ctx, |ctx| report(ctx, &params, &reason, kind, &summary))
}

pub(crate) fn one_line(report: &Value) -> String {
    let facts = [("branch", "branch"), ("sha", "sha"), ("pr", "pr")]
        .into_iter()
        .filter_map(|(field, name)| {
            report
                .get(field)
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .map(|value| {
                    let shown = if field == "sha" && value.len() > 12 {
                        &value[..12]
                    } else {
                        value
                    };
                    format!("{name}={shown}")
                })
        })
        .collect::<Vec<_>>()
        .join(" ");
    let mut line = format!(
        "{}: {}",
        report.get("kind").and_then(Value::as_str).unwrap_or(""),
        report.get("summary").and_then(Value::as_str).unwrap_or("")
    );
    if !facts.is_empty() {
        line = format!("{line} [{facts}]");
    }
    if line.chars().count() > 200 {
        line = format!("{}…", line.chars().take(199).collect::<String>());
    }
    line
}

fn report<C: Clock + 'static, I: IdFactory + 'static>(
    ctx: &AppContext<C, I>,
    params: &Value,
    reason: &str,
    kind: &str,
    summary: &str,
) -> Result<Value, VogtError> {
    let own = own_engine_session(ctx)?;
    let engine_id = reporting_session(
        ctx,
        params.get("id").and_then(Value::as_str),
        own.as_deref(),
    )?;
    let clean = |name: &str| {
        params
            .get(name)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(redact)
    };
    let list = |name: &str| {
        params
            .get(name)
            .and_then(Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(redact)
                    .collect()
            })
            .unwrap_or_default()
    };
    let fields = SessionReportFields {
        branch: clean("branch"),
        // The SHA is a commit id, not text: it is left as given.
        sha: params
            .get("sha")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string),
        pr: clean("pr"),
        checks: list("checks"),
        blockers: list("blockers"),
        next_step: clean("next_step"),
    };
    let report_id = next_id(&ctx.id_factory, "rpt");
    let comment_id = next_id(&ctx.id_factory, "cmt");
    let now = now_of(&ctx.clock);
    let wanted_work = params
        .get("work_item")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let mut writing = write_of(ctx);
    audited_write(&mut writing, OPERATION, reason, move |txn, actor| {
        let work_item_id = match wanted_work.as_deref() {
            Some(reference) => Some(resolve::work_item(txn, reference)?.id),
            None => txn
                .session_by_engine_id(&engine_id)?
                .and_then(|session| session.work_item_id),
        };
        let row = SessionReport {
            id: report_id.clone(),
            actor_id: actor.id.clone(),
            engine_session_id: Some(engine_id.clone()),
            work_item_id: work_item_id.clone(),
            kind: kind.to_string(),
            summary: summary.to_string(),
            fields,
            created_at: now,
        };
        txn.insert_session_report(&row)?;
        if let Some(work_item_id) = work_item_id.as_deref() {
            txn.insert_comment(&Comment {
                id: comment_id,
                work_item_id: work_item_id.to_string(),
                actor_id: actor.id.clone(),
                actor_display_name: actor.display_name.clone(),
                body: comment_body(&row),
                created_at: now,
                kind: "note".to_string(),
                handoff: None,
            })?;
        }
        let view = view_of(txn, &row)?;
        Ok(WriteOutcome::new(
            json!({"report": view}),
            "session_report",
            &row.id,
            serde_json::to_value(&row).unwrap_or(Value::Null),
            EVENT,
            json!({"report_id": row.id, "kind": row.kind, "engine_session_id": engine_id, "work_item": view["work_item"], "summary": summary.chars().take(200).collect::<String>()}),
        ))
    })
}

fn own_engine_session<C: Clock, I: IdFactory>(
    ctx: &AppContext<C, I>,
) -> Result<Option<String>, VogtError> {
    let reference = ctx.principal.identity_ref.as_str();
    if let Some(id) = reference.strip_prefix("agent:engine:") {
        return Ok(Some(id.to_string()));
    }
    if let Some(id) = reference.strip_prefix("agent:session:") {
        return Ok(ctx
            .declared
            .read()?
            .session_by_id(id)?
            .map(|session| session.engine_session_id));
    }
    Ok(None)
}

fn reporting_session<C: Clock, I: IdFactory>(
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
    let target = if wanted.starts_with("ses_") {
        ctx.declared
            .read()?
            .session_by_id(wanted)?
            .map(|session| session.engine_session_id)
            .ok_or_else(|| {
                VogtError::InvalidRequest(format!("no session {}", crate::core::py_repr(wanted)))
            })?
    } else {
        wanted.to_string()
    };
    if own.is_some_and(|own| own != target) {
        return Err(VogtError::ReportRefused(
            "a session reports only for itself".to_string(),
        ));
    }
    if own.is_none() && ctx.principal.kind == ActorKind::Agent {
        return Err(VogtError::ReportRefused(
            "an agent that is not a session cannot report for one".to_string(),
        ));
    }
    Ok(target)
}

pub(crate) fn view_of(view: &impl ReadView, row: &SessionReport) -> Result<Value, VogtError> {
    let work_item = match row.work_item_id.as_deref() {
        Some(id) => view.work_item_by_id(id)?.map(|item| item.reference),
        None => None,
    };
    let reporter = view
        .actor_by_id(&row.actor_id)?
        .map(|actor| actor.identity_ref);
    Ok(
        json!({"id": row.id, "kind": row.kind, "summary": row.summary, "session_id": row.engine_session_id, "work_item": work_item, "branch": row.fields.branch, "sha": row.fields.sha, "pr": row.fields.pr, "checks": row.fields.checks, "blockers": row.fields.blockers, "next_step": row.fields.next_step, "reporter": reporter, "created_at": row.created_at.to_json()}),
    )
}

fn comment_body(row: &SessionReport) -> String {
    let mut lines = vec![format!("Session report ({}): {}", row.kind, row.summary)];
    let facts = [
        ("branch", row.fields.branch.as_deref()),
        ("sha", row.fields.sha.as_deref()),
        ("pr", row.fields.pr.as_deref()),
    ]
    .into_iter()
    .filter_map(|(name, value)| value.map(|value| format!("{name}: {value}")))
    .collect::<Vec<_>>();
    if !facts.is_empty() {
        lines.push(facts.join(" · "));
    }
    if !row.fields.checks.is_empty() {
        lines.push(format!("checks: {}", row.fields.checks.join("; ")));
    }
    if !row.fields.blockers.is_empty() {
        lines.push(format!("blockers: {}", row.fields.blockers.join("; ")));
    }
    if let Some(next) = row.fields.next_step.as_deref() {
        lines.push(format!("next: {next}"));
    }
    if let Some(session) = row.engine_session_id.as_deref() {
        lines.push(format!("session: {session}"));
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::application::context::build_context;
    use crate::application::writes::audited_write;
    use crate::core::{ActorKind, CodingSession, Moment, Principal, StepClock};
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

    fn seed(built: &crate::application::context::Built) {
        crate::with_ctx!(built, |ctx| {
            let mut write = crate::application::context::write_of(ctx);
            audited_write(&mut write, "seed", "test", |txn, actor| {
                txn.insert_project(&crate::core::Project::new(
                    "prj_1",
                    "proj",
                    "Proj",
                    "/work",
                    Moment::from_unix(1_700_000_000, 0),
                ))?;
                txn.insert_session(&CodingSession {
                    id: "ses_0001".to_string(),
                    engine_session_id: "eng-1".to_string(),
                    project_id: "prj_1".to_string(),
                    work_item_id: None,
                    actor_id: actor.id.clone(),
                    cwd: "/work".to_string(),
                    template: None,
                    model: None,
                    effort: None,
                    reason: "test".to_string(),
                    purpose: None,
                    started_by_session: None,
                    started_by_kind: None,
                    started_at: Moment::from_unix(1_700_000_000, 0),
                    stopped_at: None,
                })?;
                Ok(crate::application::writes::WriteOutcome {
                    result: json!({}),
                    entity_kind: "session".to_string(),
                    entity_id: "ses_0001".to_string(),
                    payload: json!({}),
                    event_kind: "session.started".to_string(),
                    summary: json!({}),
                })
            })
        })
        .unwrap();
    }

    fn session_principal() -> Principal {
        Principal::new("agent:session:ses_0001", ActorKind::Agent, "Session").unwrap()
    }

    #[test]
    fn a_session_creates_and_reads_its_own_report() {
        let built = fresh("own", session_principal());
        seed(&built);
        let reported = super::report_op(
            &built,
            json!({
                "kind": "progress",
                "summary": "  landed the port  ",
                "branch": " wi-2143 ",
                "sha": "abc123def456",
                "checks": ["cargo test"],
                "reason": "status",
            }),
        )
        .unwrap();
        let report = &reported["report"];
        assert_eq!(report["kind"], json!("progress"));
        assert_eq!(report["summary"], json!("landed the port"));
        assert_eq!(report["session_id"], json!("eng-1"));
        assert_eq!(report["branch"], json!("wi-2143"));
        assert_eq!(report["sha"], json!("abc123def456"));
        assert_eq!(report["reporter"], json!("agent:session:ses_0001"));
        let id = report["id"].as_str().unwrap().to_string();
        let stored = crate::with_ctx!(built, |ctx| ctx
            .declared
            .read()
            .unwrap()
            .list_session_reports(10))
        .unwrap();
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].id, id);
        assert_eq!(stored[0].engine_session_id.as_deref(), Some("eng-1"));
        assert_eq!(stored[0].summary, "landed the port");
    }

    #[test]
    fn a_non_session_agent_cannot_report_another_session() {
        let principal = Principal::new("agent:oversight:other", ActorKind::Agent, "Other").unwrap();
        let built = fresh("other", principal);
        seed(&built);
        let error = super::report_op(
            &built,
            json!({
                "id": "eng-1",
                "kind": "blocker",
                "summary": "not mine",
                "reason": "probe",
            }),
        )
        .unwrap_err();
        assert!(matches!(error, VogtError::ReportRefused(_)));
    }
}
