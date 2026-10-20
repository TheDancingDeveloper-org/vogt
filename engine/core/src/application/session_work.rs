//! Binding a session to the work item it serves. Ports `bind_session_work`
//! from `src/vogt/application/services/sessions.py` (WI-998).
//!
//! One current item per session, re-declarable, and the audit log is the
//! history rather than a second table. Binding grants nothing, so any
//! `work.write` principal may: a person, the session itself, its overseer.
//! It never moves the terminal and never moves the item's state.
//!
//! The core row is the truth and is written first. The engine carries a copy
//! as its `work_item` label afterwards, and a label it could not take is
//! reported in `engine_label`, not fatal. A session Vogt never linked has no
//! row, so the label is its only binding.

use serde_json::{json, Value};

use crate::adapters::engine::EngineSession;
use crate::application::context::{write_of, AppContext, Built};
use crate::application::services::sessions::SessionSummary;
use crate::application::services::{now_of, resolve_session};
use crate::application::writes::{audited_action, audited_write, WriteOutcome};
use crate::core::{Clock, CodingSession, IdFactory};
use crate::errors::VogtError;
use crate::storage::interface::{DeclaredStore, ReadView, WriteTxn};

const BIND: &str = "session.bind_work";
const BOUND: &str = "session.work_bound";
const UNBOUND: &str = "session.work_unbound";

pub fn bind_work_op(ctx: &Built, params: Value) -> Result<Value, VogtError> {
    let reason = text(&params, "reason")
        .ok_or_else(|| VogtError::InvalidRequest("session.bind_work needs a reason".to_string()))?;
    // Do not trim `id` here. Python treats a whitespace-only id as given
    // (`params.id or own_session`), and the shared resolver is what then
    // refuses it; trimming first would fall through to the own-session path.
    let id = params.get("id").and_then(Value::as_str).map(str::to_string);
    // `work_item` distinguishes absent from null only in that both unbind:
    // Python treats a missing value and an explicit null the same.
    let work_item = match params.get("work_item") {
        Some(Value::String(reference)) => Some(reference.clone()),
        Some(Value::Null) | None => None,
        Some(_) => {
            return Err(VogtError::InvalidRequest(
                "session.bind_work work_item must be a string or null".to_string(),
            ))
        }
    };
    crate::with_ctx!(ctx, |ctx| bind(
        ctx,
        id.as_deref(),
        work_item.as_deref(),
        &reason
    ))
}

fn bind<C: Clock + 'static, I: IdFactory + 'static>(
    ctx: &AppContext<C, I>,
    id: Option<&str>,
    work_item: Option<&str>,
    reason: &str,
) -> Result<Value, VogtError> {
    let reason = crate::application::writes::validate_reason(reason)?;
    let reference = match id {
        Some(id) => id.to_string(),
        None => own_session(ctx)?,
    };
    let engine_id = resolve_session(ctx, &reference)?;
    let session = ctx.declared.read()?.session_by_engine_id(&engine_id)?;

    let wanted = work_item.map(str::trim).filter(|item| !item.is_empty());
    let item = match wanted {
        Some(reference) => Some(crate::application::resolve::work_item(
            &ctx.declared.read()?,
            reference,
        )?),
        None => None,
    };

    if session.is_none() {
        return bind_unlinked(ctx, &engine_id, item.as_ref(), &reason);
    }
    let session = session.unwrap();

    let item_id = item.as_ref().map(|item| item.id.clone());
    let item_ref = item.as_ref().map(|item| item.reference.clone());
    let item_project = item.as_ref().and_then(|item| item.project_id.clone());
    let template = ctx.config.branch_binding_template.clone();
    let session_id = session.id.clone();

    let mut write = write_of(ctx);
    let clock = std::sync::Arc::clone(write.clock());
    let recorded = audited_write(&mut write, BIND, &reason, |txn, _actor| {
        let current = txn.session_by_id(&session_id)?.ok_or_else(|| {
            VogtError::NotFound(format!("no session {}", crate::core::py_repr(&session_id)))
        })?;
        if current.stopped_at.is_some() {
            return Err(VogtError::Conflict(format!(
                "session {session_id} was stopped; its work item is a record of what it was on, \
                 not something to re-declare"
            )));
        }
        let previous = match current.work_item_id.as_deref() {
            Some(id) => txn.work_item_by_id(id)?,
            None => None,
        };
        txn.set_session_work_item(&current.id, item_id.as_deref())?;
        let declared_branch = match &item_ref {
            Some(work_ref) => Some(record_declared_branch(
                txn,
                work_ref,
                item_project.as_deref().unwrap_or(&current.project_id),
                &template,
                now_of(&clock),
            )?),
            None => None,
        };
        let updated = txn.session_by_id(&current.id)?.expect("written just now");
        let mismatch =
            item_project.as_deref() != Some(current.project_id.as_str()) && item_ref.is_some();
        let previous_ref = previous.as_ref().map(|item| item.reference.clone());
        Ok(WriteOutcome {
            result: json!({
                "previous_work_item": previous_ref,
                "project_mismatch": mismatch,
            }),
            entity_kind: "session".to_string(),
            entity_id: updated.id.clone(),
            // The post-write row, plus the item it was bound to before this
            // write. `work_item_id` inside the payload is the new one.
            payload: {
                let mut payload = audited_payload(&updated);
                payload
                    .as_object_mut()
                    .expect("a payload is an object")
                    .insert(
                        "previous_work_item_id".to_string(),
                        match &current.work_item_id {
                            Some(id) => Value::String(id.clone()),
                            None => Value::Null,
                        },
                    );
                payload
            },
            event_kind: if item_ref.is_none() { UNBOUND } else { BOUND }.to_string(),
            summary: json!({
                "engine_session_id": updated.engine_session_id,
                "work_item": item_ref,
                "previous_work_item": previous_ref,
                "branch": declared_branch,
                "project_mismatch": mismatch,
            }),
        })
    })?;

    // After the declared write, never inside it: the row is the truth and the
    // engine's label copies it. A label the engine cannot take is reported.
    let (label, detail, live) = match engine_label(ctx, &engine_id, item_ref.as_deref()) {
        Ok(outcome) => outcome,
        Err(error) => ("unavailable".to_string(), Some(error.to_string()), None),
    };
    // Re-read after the commit: the summary shows the row as written, with the
    // engine's live fields over it. `alive` stays null when the engine could
    // not be asked, which is a different fact from "not running".
    let updated = ctx
        .declared
        .read()?
        .session_by_id(&session_id)?
        .expect("written just now");
    let summary = summarize_asked(ctx, &updated, live.as_ref(), label != "unavailable")?;
    Ok(json!({
        "session": summary,
        "previous_work_item": recorded["previous_work_item"],
        "project_mismatch": recorded["project_mismatch"],
        "engine_label": label,
        "engine": detail,
    }))
}

/// A session the GUI started has no row, so the engine's label is the binding.
fn bind_unlinked<C: Clock + 'static, I: IdFactory + 'static>(
    ctx: &AppContext<C, I>,
    engine_id: &str,
    item: Option<&crate::core::WorkItem>,
    reason: &str,
) -> Result<Value, VogtError> {
    let engine = engine_of(ctx)?;
    let current = engine.get_session(engine_id)?.ok_or_else(|| {
        VogtError::NotFound(format!("no session {}", crate::core::py_repr(engine_id)))
    })?;
    let item_ref = item.map(|item| item.reference.clone());
    let updated = engine
        .set_work_item(engine_id, item_ref.as_deref())?
        .ok_or_else(|| {
            VogtError::NotFound(format!(
                "the engine cannot label session {} with a work item (it predates session work \
                 items)",
                crate::core::py_repr(engine_id)
            ))
        })?;
    let previous = current.work_item.clone();
    let mut write = write_of(ctx);
    let outcome = json!({
        "engine_session_id": engine_id,
        "linked": false,
        "work_item_id": item.map(|item| item.id.clone()),
        "previous_work_item": previous,
    });
    let summary = json!({
        "engine_session_id": engine_id,
        "linked": false,
        "work_item": item_ref,
        "previous_work_item": previous,
    });
    audited_action(
        &mut write,
        BIND,
        reason,
        "session",
        engine_id,
        &outcome,
        if item.is_none() { UNBOUND } else { BOUND },
        Some(&summary),
    )?;
    let mut session = unlinked_summary(ctx, &updated)?;
    // The summary shows the label as it stands after the write, not the
    // engine's copy of the item it was bound to before.
    session.work_item = item_ref;
    Ok(json!({
        "session": session,
        "previous_work_item": previous,
        "project_mismatch": false,
        "engine_label": "written",
        "engine": Value::Null,
    }))
}

/// Copy the binding onto the engine's label. `Ok` carries the label state,
/// what the engine said when it was not written, and the live session the
/// summary merges — falling back to a plain read when the label write itself
/// found nothing, so the summary still has live fields.
fn engine_label<C: Clock, I: IdFactory>(
    ctx: &AppContext<C, I>,
    engine_id: &str,
    item_ref: Option<&str>,
) -> Result<(String, Option<String>, Option<EngineSession>), VogtError> {
    let Some(engine) = ctx.engine.as_ref() else {
        return Ok((
            "unavailable".to_string(),
            Some("no session engine is configured (VOGT_ENGINE_URL is unset)".to_string()),
            None,
        ));
    };
    match engine.set_work_item(engine_id, item_ref)? {
        Some(live) => Ok(("written".to_string(), None, Some(live))),
        None => Ok((
            "not_found".to_string(),
            Some(
                "the engine has no such session, or predates session work items; the binding is \
                 recorded in Vogt"
                    .to_string(),
            ),
            engine.get_session(engine_id)?,
        )),
    }
}

/// What the audit row records about a session, plus nothing about the
/// credential. The same shape `sessions` writes, duplicated because that copy
/// is private to its module.
fn audited_payload(session: &CodingSession) -> Value {
    json!({
        "id": session.id,
        "engine_session_id": session.engine_session_id,
        "project_id": session.project_id,
        "work_item_id": session.work_item_id,
        "actor_id": session.actor_id,
        "cwd": session.cwd,
        "template": session.template,
        "model": session.model,
        "effort": session.effort,
        "started_at": session.started_at.to_iso(),
        "stopped_at": session.stopped_at.map(|moment| moment.to_iso()),
    })
}

/// One linked session as the list shows it. `asked` is whether the engine was
/// reached: `alive` is its answer then, and null when it could not be asked.
fn summarize_asked<C: Clock, I: IdFactory>(
    ctx: &AppContext<C, I>,
    session: &CodingSession,
    live: Option<&EngineSession>,
    asked: bool,
) -> Result<SessionSummary, VogtError> {
    let view = ctx.declared.read()?;
    let project = view.project_by_id(&session.project_id)?;
    let work_item = match session.work_item_id.as_deref() {
        Some(id) => view.work_item_by_id(id)?,
        None => None,
    };
    let actor = view.actor_by_id(&session.actor_id)?;
    let mut summary = SessionSummary {
        id: session.id.clone(),
        engine_session_id: session.engine_session_id.clone(),
        linked: true,
        project: project.map(|project| project.slug),
        work_item: work_item.as_ref().map(|item| item.reference.clone()),
        work_item_title: work_item.as_ref().map(|item| item.title.clone()),
        work_item_state: work_item.map(|item| item.state.to_string()),
        actor: Some(
            actor
                .map(|actor| actor.identity_ref)
                .unwrap_or(session.actor_id.clone()),
        ),
        cwd: session.cwd.clone(),
        template: session.template.clone(),
        model: session.model.clone(),
        effort: session.effort.clone(),
        reason: Some(session.reason.clone()),
        purpose: session.purpose.clone(),
        started_by_session: session.started_by_session.clone(),
        started_by_kind: session.started_by_kind.clone(),
        started_at: Some(session.started_at),
        stopped_at: session.stopped_at,
        name: None,
        command: None,
        exit_code: None,
        activity_changed_at: None,
        conversation_agent: None,
        activity: None,
        alive: asked.then(|| live.is_some_and(|live| live.alive)),
        turn_started_at: None,
        last_output_at: None,
        done_at: None,
        approval: None,
        blocked: None,
        hibernation: None,
        keep_awake: None,
        autopilot: None,
        autopilot_nudges: None,
        role: None,
        conversation_id: None,
        resources: None,
        permission_mode: None,
        stopped_by: None,
        stop_reason: None,
        last_reply_excerpt: None,
        running: None,
    };
    if let Some(live) = live {
        apply_live(&mut summary, live);
    }
    Ok(summary)
}

/// A session the engine has and Vogt never recorded: the engine's id in both
/// id fields, and the item's title and state when its ref resolves.
fn unlinked_summary<C: Clock, I: IdFactory>(
    ctx: &AppContext<C, I>,
    session: &EngineSession,
) -> Result<SessionSummary, VogtError> {
    let item = match session.work_item.as_deref() {
        Some(reference) => ctx.declared.read()?.work_item_by_ref(reference)?,
        None => None,
    };
    let mut summary = SessionSummary {
        id: session.id.clone(),
        engine_session_id: session.id.clone(),
        linked: false,
        project: None,
        work_item: session.work_item.clone(),
        work_item_title: item.as_ref().map(|item| item.title.clone()),
        work_item_state: item.map(|item| item.state.to_string()),
        actor: None,
        cwd: session.cwd.clone(),
        template: None,
        model: None,
        effort: None,
        reason: None,
        purpose: None,
        started_by_session: None,
        started_by_kind: None,
        started_at: session
            .created_at
            .as_deref()
            .and_then(|text| crate::core::from_iso(text).ok()),
        stopped_at: None,
        name: None,
        command: None,
        exit_code: None,
        activity_changed_at: None,
        conversation_agent: None,
        activity: None,
        alive: Some(session.alive),
        turn_started_at: None,
        last_output_at: None,
        done_at: None,
        approval: None,
        blocked: None,
        hibernation: None,
        keep_awake: None,
        autopilot: None,
        autopilot_nudges: None,
        role: None,
        conversation_id: None,
        resources: None,
        permission_mode: None,
        stopped_by: None,
        stop_reason: None,
        last_reply_excerpt: None,
        running: None,
    };
    apply_live(&mut summary, session);
    Ok(summary)
}

/// Copy the engine's live fields onto a summary.
fn apply_live(summary: &mut SessionSummary, live: &EngineSession) {
    use crate::application::services::sessions::{
        SessionApproval, SessionApprovalOption, SessionBlocked, SessionHibernation,
        SessionResources,
    };
    summary.activity = Some(live.activity.clone());
    summary.name = Some(live.name.clone());
    summary.command = live.command.clone();
    summary.exit_code = live.exit_code;
    summary.activity_changed_at = live.activity_changed_at.clone();
    summary.conversation_agent = live.conversation_agent.clone();
    summary.turn_started_at = live.turn_started_at.clone();
    summary.last_output_at = live.last_output_at.clone();
    summary.done_at = live.done_at.clone();
    summary.approval = live.approval.as_ref().map(|approval| SessionApproval {
        question: approval.question.clone(),
        command_excerpt: approval.command_excerpt.clone(),
        kind: approval.kind.clone(),
        options: approval
            .options
            .iter()
            .map(|(number, label, selected)| SessionApprovalOption {
                number: *number,
                label: label.clone(),
                selected: *selected,
            })
            .collect(),
        deadline_seconds: approval.deadline_seconds,
        deadline_at: approval.deadline_at.clone(),
        detected_at: (!approval.detected_at.is_empty()).then(|| approval.detected_at.clone()),
    });
    summary.blocked = live.blocked.as_ref().map(|blocked| SessionBlocked {
        blocker: blocked.reason.clone(),
        items: blocked.items.clone(),
        since: blocked.since.clone(),
    });
    summary.hibernation = live
        .hibernation
        .as_ref()
        .map(|hibernation| SessionHibernation {
            at: hibernation.at.clone(),
            trigger: hibernation.trigger.clone(),
            resumable: hibernation.resumable,
            reason: hibernation.reason.clone(),
        });
    summary.keep_awake = Some(live.keep_awake);
    summary.autopilot = Some(live.autopilot);
    summary.autopilot_nudges = Some(live.autopilot_nudges);
    summary.role = Some(live.role.clone());
    summary.conversation_id = live.conversation_id.clone();
    summary.resources = live.resources.as_ref().map(|resources| SessionResources {
        rss_bytes: resources.rss_bytes,
        cpu_pct: resources.cpu_pct,
        processes: resources.processes,
        sampled_at: resources.sampled_at.clone(),
        over_threshold: resources.over_threshold,
    });
    summary.permission_mode = live.permission_mode.clone();
    summary.stopped_by = live.stopped_by.clone();
    summary.stop_reason = live.stop_reason.clone();
}

fn engine_of<C: Clock, I: IdFactory>(
    ctx: &AppContext<C, I>,
) -> Result<&crate::adapters::engine::EngineClient, VogtError> {
    ctx.engine.as_ref().ok_or_else(|| {
        VogtError::EngineUnavailable(
            "no session engine is configured, so there is nothing to open a terminal on \
             (set VOGT_ENGINE_URL)"
                .to_string(),
        )
    })
}

/// The session a session's own token belongs to, for an omitted `id`.
fn own_session<C: Clock, I: IdFactory>(ctx: &AppContext<C, I>) -> Result<String, VogtError> {
    ctx.principal
        .identity_ref
        .strip_prefix("agent:session:")
        .filter(|id| !id.is_empty())
        .map(str::to_string)
        .ok_or_else(|| {
            VogtError::InvalidRequest(
                "give `id`: this caller is not a session Vogt started, so it does not name one \
                 (inside a session, pass $VOGT_ENGINE_SESSION_ID)"
                    .to_string(),
            )
        })
}

/// Record the item's declared branch, as a start does. An unbind leaves it.
fn record_declared_branch(
    txn: &mut impl WriteTxn,
    work_ref: &str,
    project_id: &str,
    template: &str,
    at: crate::core::Moment,
) -> Result<String, VogtError> {
    let branch = crate::branches::default_branch_name(work_ref, template);
    let existing = txn.work_overlay(work_ref)?;
    let mut branches = existing
        .as_ref()
        .map(|row| row.branches.clone())
        .unwrap_or_default();
    if branches.iter().any(|have| have == &branch) {
        return Ok(branch);
    }
    branches.push(branch.clone());
    let overlay = match existing {
        Some(mut row) => {
            row.branches = branches;
            row.updated_at = at;
            row
        }
        None => crate::core::WorkOverlay {
            subject_key: work_ref.to_string(),
            project_id: project_id.to_string(),
            rank: None,
            workflow_state: None,
            priority: None,
            effort: None,
            assignee_actor_id: None,
            initiative_id: None,
            branches,
            created_at: at,
            updated_at: at,
        },
    };
    txn.upsert_work_overlay(&overlay)?;
    Ok(branch)
}

fn text(params: &Value, name: &str) -> Option<String> {
    params
        .get(name)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use serde_json::{json, Value};

    use crate::adapters::engine::EngineClient;
    use crate::application::context::build_context;
    use crate::application::writes::audited_write;
    use crate::core::{
        ActorKind, CodingSession, Moment, Origin, Principal, Priority, Project, StepClock,
        TrustState, WorkItem, WorkKind,
    };
    use crate::errors::VogtError;
    use crate::storage::interface::{DeclaredStore, ReadView, WriteTxn};

    fn fresh(name: &str) -> crate::application::context::Built {
        let dir = std::env::temp_dir().join(format!("vogt-bind-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut clock = None;
        let mut ids = None;
        crate::application::instance::init(&dir, &mut clock, &mut ids).unwrap();
        let engine = EngineClient::new(
            "http://engine",
            None,
            Some(std::sync::Arc::new(|path, _, body, method| {
                let answer = if method == "GET" {
                    format!(r#"{{"id":"{path}","alive":true,"role":"worker"}}"#).into_bytes()
                } else {
                    body.to_vec()
                };
                (200, answer)
            })),
        );
        build_context(
            crate::config::VogtConfig {
                data_dir: dir,
                ..crate::config::VogtConfig::default()
            },
            Some(Principal::new("local:ada", ActorKind::Human, "Ada").unwrap()),
            Some(StepClock::new(Moment::from_unix(1_700_000_000, 0))),
            None,
            None,
            Some(engine),
            None,
            None,
        )
        .unwrap()
    }

    /// A project, a work item and a live session, so a bind has something real
    /// to point at.
    fn seed(built: &crate::application::context::Built, project: &str) {
        let project = project.to_string();
        crate::with_ctx!(built, |ctx| {
            let mut write = crate::application::context::write_of(ctx);
            audited_write(&mut write, "seed", "test", move |txn, actor| {
                txn.insert_project(&Project::new(
                    "prj_1",
                    "proj",
                    "Proj",
                    "/work",
                    Moment::from_unix(1_700_000_000, 0),
                ))?;
                if project != "prj_1" {
                    txn.insert_project(&Project::new(
                        &project,
                        "other",
                        "Other",
                        "/other",
                        Moment::from_unix(1_700_000_000, 0),
                    ))?;
                }
                txn.insert_work_item(&WorkItem {
                    id: "wrk_1".to_string(),
                    reference: "WI-7".to_string(),
                    kind: WorkKind::Feature,
                    title: "The thing".to_string(),
                    body: String::new(),
                    state: "open".to_string(),
                    priority: Priority::P2,
                    effort: None,
                    project_id: Some(project.clone()),
                    project_slug: Some("proj".to_string()),
                    initiative_id: None,
                    origin: Origin::Created,
                    trust_state: TrustState::Unverified,
                    assignee_actor_id: None,
                    assignee_identity_ref: None,
                    labels: Vec::new(),
                    relations: Vec::new(),
                    superseded_by: None,
                    created_at: Moment::from_unix(1_700_000_000, 0),
                    updated_at: Moment::from_unix(1_700_000_000, 0),
                })?;
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

    #[test]
    fn binding_records_the_item_and_labels_the_engine() {
        let built = fresh("bind");
        seed(&built, "prj_1");
        let bound = super::bind_work_op(
            &built,
            json!({"id": "ses_0001", "work_item": "WI-7", "reason": "working it"}),
        )
        .unwrap();
        assert_eq!(bound["previous_work_item"], json!(null));
        assert_eq!(bound["project_mismatch"], json!(false));
        assert_eq!(bound["engine_label"], "written");
        assert_eq!(bound["session"]["id"], "ses_0001");
        assert_eq!(bound["session"]["linked"], json!(true));
        assert_eq!(bound["session"]["project"], "proj");
        assert_eq!(bound["session"]["work_item"], "WI-7");
        assert_eq!(bound["session"]["work_item_title"], "The thing");
        assert_eq!(bound["session"]["actor"], "local:ada");
        assert_eq!(bound["session"]["alive"], json!(true));
        assert_eq!(
            bound.as_object().unwrap().keys().next().unwrap(),
            "session",
            "the summary leads the answer"
        );
        let ledger = crate::with_ctx!(&built, |ctx| {
            ctx.declared
                .read()
                .unwrap()
                .list_audit(&crate::storage::interface::AuditQuery {
                    limit: 20,
                    offset: 0,
                    actor_id: None,
                    operation: Some("session.bind_work".to_string()),
                    entity_id: None,
                    project_id: None,
                    since: None,
                    until: None,
                })
        })
        .unwrap();
        let row = ledger
            .iter()
            .find(|row| row.reason == "working it")
            .unwrap();
        let digest = crate::decisions::digest_of(&json!({
            "actor_id": row.actor_id,
            "cwd": "/work",
            "effort": null,
            "engine_session_id": "eng-1",
            "id": "ses_0001",
            "model": null,
            "previous_work_item_id": null,
            "project_id": "prj_1",
            "started_at": "2023-11-14T22:13:20+00:00",
            "stopped_at": null,
            "template": null,
            "work_item_id": "wrk_1",
        }));
        assert_eq!(row.payload_digest, digest);
        let again = super::bind_work_op(
            &built,
            json!({"id": "ses_0001", "work_item": null, "reason": "done"}),
        )
        .unwrap();
        assert_eq!(again["previous_work_item"], "WI-7");
    }

    #[test]
    fn an_item_in_another_project_is_bound_and_flagged() {
        let built = fresh("mismatch");
        seed(&built, "prj_other");
        let bound = super::bind_work_op(
            &built,
            json!({"id": "ses_0001", "work_item": "WI-7", "reason": "across"}),
        )
        .unwrap();
        assert_eq!(bound["project_mismatch"], json!(true));
        assert_eq!(bound["engine_label"], "written");
    }

    #[test]
    fn a_blank_id_is_given_not_omitted() {
        let built = fresh("blank");
        let error = super::bind_work_op(
            &built,
            json!({"id": "   ", "work_item": "WI-7", "reason": "blank"}),
        )
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            "a session id is required (ses_… or the engine's session UUID)"
        );
    }

    #[test]
    fn a_caller_who_is_not_a_session_must_name_one() {
        let built = fresh("noid");
        let error = super::bind_work_op(&built, json!({"work_item": "WI-7", "reason": "mine"}))
            .unwrap_err();
        assert!(matches!(error, VogtError::InvalidRequest(_)), "{error:?}");
    }

    /// What the stand-in engine was asked, and the work-item label it holds
    /// for each session. `knows_work_items` false answers the label route with
    /// a 404, as an engine that predates session work items does.
    #[derive(Default)]
    struct StandIn {
        asked: Vec<(String, String, Value)>,
        labels: std::collections::BTreeMap<String, String>,
        knows_work_items: bool,
    }

    type Shared = std::sync::Arc<std::sync::Mutex<StandIn>>;

    fn stand_in(shared: &Shared) -> EngineClient {
        let shared = std::sync::Arc::clone(shared);
        EngineClient::new(
            "http://engine",
            None,
            Some(std::sync::Arc::new(move |path, _, body, method| {
                let route = path.split("/api/").nth(1).unwrap_or(path).to_string();
                let payload: Value = if body.is_empty() {
                    Value::Null
                } else {
                    serde_json::from_slice(body).unwrap_or(Value::Null)
                };
                let mut state = shared.lock().unwrap();
                state
                    .asked
                    .push((method.to_string(), route.clone(), payload.clone()));
                let parts: Vec<&str> = route.split('/').collect();
                let Some(engine_id) = parts.get(1).filter(|_| parts[0] == "sessions") else {
                    return (404, Vec::new());
                };
                let engine_id = engine_id.to_string();
                let summary = |state: &StandIn| {
                    let mut row = json!({"id": engine_id, "name": engine_id, "alive": true,
                        "activity": "idle", "cwd": "/work", "role": "worker"});
                    if let Some(label) = state.labels.get(&engine_id) {
                        row["work_item"] = json!(label);
                    }
                    serde_json::to_vec(&row).unwrap()
                };
                match (method, parts.get(2).copied()) {
                    ("GET", None) => (200, summary(&state)),
                    ("POST", Some("work-item")) if state.knows_work_items => {
                        match payload["work_item"].as_str() {
                            Some(label) => {
                                state.labels.insert(engine_id.clone(), label.to_string());
                            }
                            None => {
                                state.labels.remove(&engine_id);
                            }
                        }
                        (200, summary(&state))
                    }
                    _ => (404, Vec::new()),
                }
            })),
        )
    }

    fn work_item(id: &str, reference: &str, project_id: &str) -> WorkItem {
        WorkItem {
            id: id.to_string(),
            reference: reference.to_string(),
            kind: WorkKind::Feature,
            title: "The thing".to_string(),
            body: String::new(),
            state: "open".to_string(),
            priority: Priority::P2,
            effort: None,
            project_id: Some(project_id.to_string()),
            project_slug: Some("proj".to_string()),
            initiative_id: None,
            origin: Origin::Created,
            trust_state: TrustState::Unverified,
            assignee_actor_id: None,
            assignee_identity_ref: None,
            labels: Vec::new(),
            relations: Vec::new(),
            superseded_by: None,
            created_at: Moment::from_unix(1_700_000_000, 0),
            updated_at: Moment::from_unix(1_700_000_000, 0),
        }
    }

    /// A Vogt instance over `dir` for one principal, asking `shared`'s engine.
    fn as_principal(
        dir: &std::path::Path,
        principal: Principal,
        shared: &Shared,
        engine: bool,
    ) -> crate::application::context::Built {
        build_context(
            crate::config::VogtConfig {
                data_dir: dir.to_path_buf(),
                ..crate::config::VogtConfig::default()
            },
            Some(principal),
            Some(StepClock::new(Moment::from_unix(1_700_000_000, 0))),
            None,
            None,
            engine.then(|| stand_in(shared)),
            None,
            None,
        )
        .unwrap()
    }

    /// An instance with two projects, two work items (WI-7 in `proj`, WI-8 in
    /// `other`), a live session `ses_0001` for `eng-1`, a live child `ses_0002`
    /// for `eng-2`, and a stopped `ses_0003` for `eng-3`.
    fn estate(
        name: &str,
    ) -> (
        std::path::PathBuf,
        Shared,
        crate::application::context::Built,
    ) {
        let dir = std::env::temp_dir().join(format!("vogt-bind-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut clock = None;
        let mut ids = None;
        crate::application::instance::init(&dir, &mut clock, &mut ids).unwrap();
        let shared: Shared = std::sync::Arc::new(std::sync::Mutex::new(StandIn {
            knows_work_items: true,
            ..StandIn::default()
        }));
        let built = as_principal(&dir, person(), &shared, true);
        crate::with_ctx!(&built, |ctx| {
            let mut write = crate::application::context::write_of(ctx);
            audited_write(&mut write, "seed", "test", |txn, actor| {
                let at = Moment::from_unix(1_700_000_000, 0);
                txn.insert_project(&Project::new("prj_1", "proj", "Proj", "/work", at))?;
                txn.insert_project(&Project::new("prj_other", "other", "Other", "/other", at))?;
                txn.insert_work_item(&work_item("wrk_7", "WI-7", "prj_1"))?;
                let mut second = work_item("wrk_8", "WI-8", "prj_other");
                second.title = "the ops item".to_string();
                txn.insert_work_item(&second)?;
                for (id, engine_id, stopped) in [
                    ("ses_0001", "eng-1", None),
                    ("ses_0002", "eng-2", None),
                    ("ses_0003", "eng-3", Some(at)),
                ] {
                    txn.insert_session(&CodingSession {
                        id: id.to_string(),
                        engine_session_id: engine_id.to_string(),
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
                        started_at: at,
                        stopped_at: stopped,
                    })?;
                }
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
        (dir, shared, built)
    }

    fn person() -> Principal {
        Principal::new("local:ada", ActorKind::Human, "Ada").unwrap()
    }

    /// The label writes the engine was sent for one session, in order.
    fn label_writes(shared: &Shared, engine_id: &str) -> Vec<Value> {
        let route = format!("sessions/{engine_id}/work-item");
        shared
            .lock()
            .unwrap()
            .asked
            .iter()
            .filter(|(method, path, _)| method.as_str() == "POST" && *path == route)
            .map(|(_, _, body)| body["work_item"].clone())
            .collect()
    }

    #[test]
    fn rebinding_and_unbinding_are_audited_and_labelled() {
        let (_, shared, built) = estate("rebind");
        let bound = super::bind_work_op(
            &built,
            json!({"id": "ses_0001", "work_item": "WI-7", "reason": "start"}),
        )
        .unwrap();
        assert_eq!(bound["previous_work_item"], json!(null));
        assert_eq!(bound["engine_label"], "written");
        assert_eq!(bound["session"]["work_item_state"], "open");

        // Rebinding by the engine's id reaches the same row; WI-8 is filed
        // under another project, so the answer says so, and the terminal stays
        // where it was.
        let rebound = super::bind_work_op(
            &built,
            json!({"id": "eng-1", "work_item": "WI-8", "reason": "move"}),
        )
        .unwrap();
        assert_eq!(rebound["previous_work_item"], "WI-7");
        assert_eq!(rebound["project_mismatch"], json!(true));
        assert_eq!(rebound["session"]["cwd"], "/work");

        let cleared = super::bind_work_op(
            &built,
            json!({"id": "ses_0001", "work_item": null, "reason": "done"}),
        )
        .unwrap();
        assert_eq!(cleared["session"]["work_item"], json!(null));
        assert_eq!(cleared["previous_work_item"], "WI-8");
        assert_eq!(
            label_writes(&shared, "eng-1"),
            vec![json!("WI-7"), json!("WI-8"), json!(null)]
        );
        assert!(!shared.lock().unwrap().labels.contains_key("eng-1"));

        let ledger = crate::with_ctx!(&built, |ctx| {
            ctx.declared
                .read()
                .unwrap()
                .list_audit(&crate::storage::interface::AuditQuery {
                    limit: 20,
                    offset: 0,
                    actor_id: None,
                    operation: Some("session.bind_work".to_string()),
                    entity_id: None,
                    project_id: None,
                    since: None,
                    until: None,
                })
        })
        .unwrap();
        assert_eq!(ledger.len(), 3, "one audit row per bind");
        assert!(ledger.iter().all(|row| row.entity_id == "ses_0001"));
    }

    #[test]
    fn the_declared_branch_is_recorded_and_the_item_names_the_session() {
        let (_, _, built) = estate("branch");
        super::bind_work_op(
            &built,
            json!({"id": "ses_0001", "work_item": "WI-7", "reason": "start"}),
        )
        .unwrap();
        let detail = crate::registry::default_registry()
            .get("work.get")
            .expect("work.get is registered")
            .run(Some(&built), json!({"ref": "WI-7"}))
            .unwrap();
        assert_eq!(detail["sessions"][0]["id"], "ses_0001");
        assert!(
            detail["branches"]
                .as_array()
                .unwrap()
                .iter()
                .any(|branch| matches!(branch["source"].as_str(), Some("declared" | "both"))),
            "{detail}"
        );
    }

    #[test]
    fn another_session_may_bind_a_child() {
        let (dir, shared, _) = estate("child");
        let overseer = as_principal(
            &dir,
            Principal::new("agent:session:ses_0001", ActorKind::Agent, "Session").unwrap(),
            &shared,
            true,
        );
        let result = super::bind_work_op(
            &overseer,
            json!({"id": "ses_0002", "work_item": "WI-7", "reason": "child"}),
        )
        .unwrap();
        assert_eq!(result["session"]["id"], "ses_0002");
        assert_eq!(result["session"]["work_item"], "WI-7");
    }

    #[test]
    fn a_session_binds_itself_without_an_id() {
        let (dir, shared, _) = estate("self");
        let own = as_principal(
            &dir,
            Principal::new("agent:session:ses_0001", ActorKind::Agent, "Session").unwrap(),
            &shared,
            true,
        );
        let result =
            super::bind_work_op(&own, json!({"work_item": "WI-7", "reason": "mine"})).unwrap();
        assert_eq!(result["session"]["id"], "ses_0001");
        assert_eq!(result["session"]["work_item"], "WI-7");
    }

    #[test]
    fn an_unknown_item_or_a_stopped_session_is_refused() {
        let (_, _, built) = estate("refused");
        let unknown = super::bind_work_op(
            &built,
            json!({"id": "ses_0001", "work_item": "WI-99", "reason": "typo"}),
        )
        .unwrap_err();
        assert!(matches!(unknown, VogtError::NotFound(_)), "{unknown:?}");
        let stopped = super::bind_work_op(
            &built,
            json!({"id": "ses_0003", "work_item": "WI-7", "reason": "late"}),
        )
        .unwrap_err();
        assert!(matches!(stopped, VogtError::Conflict(_)), "{stopped:?}");
    }

    #[test]
    fn a_label_the_engine_refuses_is_reported_not_fatal() {
        let (dir, shared, built) = estate("label");
        shared.lock().unwrap().knows_work_items = false;
        let bound = super::bind_work_op(
            &built,
            json!({"id": "ses_0001", "work_item": "WI-7", "reason": "start"}),
        )
        .unwrap();
        assert_eq!(bound["engine_label"], "not_found");
        assert!(bound["engine"].is_string(), "{bound}");
        assert_eq!(
            bound["session"]["work_item"], "WI-7",
            "the core row is the truth"
        );

        // With no engine at all the label is unavailable, not dead.
        let offline = as_principal(&dir, person(), &shared, false);
        let unbound = super::bind_work_op(
            &offline,
            json!({"id": "ses_0001", "work_item": null, "reason": "done"}),
        )
        .unwrap();
        assert_eq!(unbound["engine_label"], "unavailable");
        assert_eq!(unbound["session"]["work_item"], json!(null));
        assert_eq!(
            unbound["session"]["alive"],
            json!(null),
            "not asked is not dead"
        );
    }

    #[test]
    fn an_unlinked_session_is_bound_by_its_engine_label_only() {
        let (_, shared, built) = estate("unlinked");
        let bound = super::bind_work_op(
            &built,
            json!({"id": "eng-9", "work_item": "WI-7", "reason": "gui-started"}),
        )
        .unwrap();
        assert_eq!(bound["session"]["linked"], json!(false));
        assert_eq!(bound["session"]["work_item"], "WI-7");
        assert_eq!(bound["session"]["work_item_title"], "The thing");
        assert_eq!(shared.lock().unwrap().labels["eng-9"], "WI-7");

        let ledger = crate::with_ctx!(&built, |ctx| {
            ctx.declared
                .read()
                .unwrap()
                .list_audit(&crate::storage::interface::AuditQuery {
                    limit: 20,
                    offset: 0,
                    actor_id: None,
                    operation: Some("session.bind_work".to_string()),
                    entity_id: None,
                    project_id: None,
                    since: None,
                    until: None,
                })
        })
        .unwrap();
        assert!(ledger.iter().any(|row| row.entity_id == "eng-9"));

        shared.lock().unwrap().knows_work_items = false;
        let error = super::bind_work_op(
            &built,
            json!({"id": "eng-9", "work_item": null, "reason": "done"}),
        )
        .unwrap_err();
        assert!(matches!(error, VogtError::NotFound(_)), "{error:?}");
    }
}
