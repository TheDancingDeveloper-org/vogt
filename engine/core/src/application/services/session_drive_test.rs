//! Ports the WI-1518, WI-1834, WI-1852, WI-1854, WI-1870 and WI-1871 cases in
//! `tests/test_session_sweep.py`, `tests/test_session_hibernation.py`,
//! `tests/test_sessions.py` and `tests/test_delivery.py` that stand on the
//! service: the sweep's attention for done, awaiting-task, question-menu and
//! input-pending sessions and its stale blocked reports; input and wake
//! clearing a blocked report; the queued-input follow-up; the task a start
//! implies; a Claude model picking Claude Code; and a resumed oversight's
//! claimed engine session.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use super::sessions::{
    input, start_session, sweep_sessions, wake, InputParams, SessionSweepResult,
    StartSessionParams, SweepSessionsParams, WakeParams,
};
use super::sessions_test::{insert_project, moment, start_params};
use crate::adapters::engine::{EngineClient, Transport};
use crate::application::context::{build_context, Built};
use crate::core::{ActorKind, Principal, SequentialIds, StepClock};
use crate::delivery::tests::{boxed, claude};
use crate::errors::VogtError;
use crate::storage::interface::{AuditQuery, DeclaredStore, ObservedStore, ReadView};

const WHY: &str = "sweep test";

fn unique() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    COUNTER.fetch_add(1, Ordering::Relaxed)
}

/// `tests/test_session_sweep.py`'s `Engine`: rows it started, the list, one
/// session, a blocked report, input, and the sweep.
#[derive(Default)]
struct Rows {
    rows: Vec<(String, Value)>,
    counter: usize,
    sweep_queries: Vec<String>,
    inputs: Vec<(String, Value)>,
    specs: Vec<Value>,
    /// A sweep entry's screen tail, by engine id, in place of the default.
    tails: BTreeMap<String, Vec<String>>,
}

impl Rows {
    fn row(&mut self, engine_id: &str) -> &mut Value {
        &mut self
            .rows
            .iter_mut()
            .find(|(id, _)| id == engine_id)
            .expect("a row this engine started")
            .1
    }

    fn update(&mut self, engine_id: &str, over: Value) {
        let row = self.row(engine_id);
        for (key, value) in over.as_object().unwrap() {
            row[key] = value.clone();
        }
    }
}

fn answer(rows: &Mutex<Rows>, url: &str, body: &[u8], method: &str) -> (u16, Vec<u8>) {
    let mut rows = rows.lock().unwrap();
    let (path, query) = url
        .trim_start_matches("http://engine")
        .split_once('?')
        .map(|(path, query)| (path.to_string(), query.to_string()))
        .unwrap_or_else(|| {
            (
                url.trim_start_matches("http://engine").to_string(),
                String::new(),
            )
        });
    let ok = |value: &Value| (200, value.to_string().into_bytes());
    if method == "POST" && path == "/api/sessions" {
        rows.counter += 1;
        let id = format!("00000000-0000-4000-8000-00000000000{}", rows.counter);
        rows.specs
            .push(serde_json::from_slice(body).unwrap_or(Value::Null));
        let row = json!({
            "id": id,
            "name": id,
            "activity": "running",
            "alive": true,
            "cwd": "/work/tree",
            "created_at": "2026-10-05T00:00:00Z",
            "last_output_at": "2026-10-05T00:00:00Z",
        });
        rows.rows.push((id, row.clone()));
        return ok(&row);
    }
    if method == "GET" && path == "/api/sessions" {
        let listed: Vec<Value> = rows.rows.iter().map(|(_, row)| row.clone()).collect();
        return ok(&json!({"sessions": listed, "hidden_count": 0}));
    }
    if method == "GET" && path == "/api/sessions/sweep" {
        rows.sweep_queries.push(query);
        let entries: Vec<Value> = rows
            .rows
            .iter()
            .filter(|(_, row)| row["alive"] == json!(true) || row["activity"] == "hibernated")
            .map(|(id, row)| {
                let tail = rows
                    .tails
                    .get(id)
                    .cloned()
                    .unwrap_or_else(|| vec![format!("tail of {id}"), "> ".to_string()]);
                json!({
                    "summary": row,
                    "screen_tail": tail,
                    "ready": row["activity"] == "waiting-for-input",
                })
            })
            .collect();
        return ok(&json!(entries));
    }
    let parts: Vec<&str> = path.split('/').collect();
    if parts.len() >= 4 && parts[1] == "api" && parts[2] == "sessions" {
        let id = parts[3].to_string();
        let tail = parts.get(4).copied().unwrap_or("");
        if !rows.rows.iter().any(|(row, _)| *row == id) {
            return (404, b"{}".to_vec());
        }
        if method == "GET" && tail.is_empty() {
            return ok(rows.row(&id));
        }
        if method == "POST" && tail == "blocked" {
            let asked: Value = serde_json::from_slice(body).unwrap();
            let blocked = if asked["blocked"] == json!(true) {
                json!({
                    "reason": asked["reason"],
                    "items": asked["items"],
                    "since": "2026-10-18T09:00:00Z",
                })
            } else {
                Value::Null
            };
            rows.row(&id)["blocked"] = blocked;
            return ok(rows.row(&id));
        }
        if method == "POST" && tail == "input" {
            rows.inputs
                .push((id, serde_json::from_slice(body).unwrap_or(Value::Null)));
            return (200, b"{}".to_vec());
        }
        if method == "GET" && tail.starts_with("screen") {
            let row = rows.row(&id).clone();
            return ok(&json!({"id": id, "lines": [], "activity": row["activity"], "alive": true}));
        }
    }
    (404, b"{}".to_vec())
}

fn opened_with(transport: Transport, principal: Principal) -> Built {
    let dir = std::env::temp_dir().join(format!("vogt-drive-{}-{}", std::process::id(), unique()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let config = crate::config::VogtConfig {
        data_dir: dir,
        ..crate::config::VogtConfig::default()
    };
    let built = build_context(
        config,
        Some(principal),
        Some(StepClock::new(moment())),
        Some(SequentialIds::new(None).unwrap()),
        None,
        Some(EngineClient::new("http://engine", None, Some(transport))),
        None,
        None,
    )
    .unwrap();
    let Built::StepSequential(ctx) = &built else {
        panic!("expected a step clock");
    };
    ctx.declared.migrate().unwrap();
    ctx.declared
        .bootstrap(&Principal::new("local:test-user", ActorKind::Human, "Test").unwrap())
        .unwrap();
    ctx.observed.migrate().unwrap();
    built
}

fn person() -> Principal {
    Principal::new("local:test-user", ActorKind::Human, "Test").unwrap()
}

/// A wired instance over a fresh `Rows` engine, with the `beta` project.
fn wired_as(principal: Principal) -> (Built, Arc<Mutex<Rows>>) {
    let rows = Arc::new(Mutex::new(Rows::default()));
    let state = Arc::clone(&rows);
    let transport: Transport =
        Arc::new(move |url, _headers, body, method| answer(&state, url, body, method));
    let built = opened_with(transport, principal);
    insert_project(&built, "beta");
    (built, rows)
}

fn wired() -> (Built, Arc<Mutex<Rows>>) {
    wired_as(person())
}

fn ctx(built: &Built) -> &crate::application::context::AppContext<StepClock, SequentialIds> {
    let Built::StepSequential(ctx) = built else {
        unreachable!("opened_with builds a step clock");
    };
    ctx
}

/// Start a session on `beta`: its `ses_…` id and its engine id.
fn start(built: &Built) -> (String, String) {
    let started = start_session(ctx(built), &start_params(Some("beta"), None, WHY)).unwrap();
    (started.session.id, started.session.engine_session_id)
}

fn sweep(built: &Built, screen_lines: i64) -> SessionSweepResult {
    sweep_sessions(
        ctx(built),
        &SweepSessionsParams {
            project: None,
            screen_lines,
            stall_after_minutes: 10,
        },
    )
    .unwrap()
}

fn row_of<'a>(
    result: &'a SessionSweepResult,
    engine_id: &str,
) -> &'a super::sessions::SessionSweepRow {
    result
        .rows
        .iter()
        .find(|row| row.session.engine_session_id == engine_id)
        .expect("a row for the session")
}

fn needs_you(result: &SessionSweepResult) -> i64 {
    result.counts["needs_you"].as_i64().unwrap()
}

fn report_blocked(built: &Built, id: &str, blocker: &str) {
    crate::application::session_blocked::report_blocked_op(
        built,
        json!({"id": id, "blocker": blocker, "reason": WHY}),
    )
    .unwrap();
}

fn unblocked_reasons(built: &Built, entity: &str) -> Vec<String> {
    let view = ctx(built).declared.read().unwrap();
    view.list_audit(&AuditQuery {
        limit: 50,
        offset: 0,
        actor_id: None,
        operation: Some("session.report_unblocked".to_string()),
        entity_id: Some(entity.to_string()),
        project_id: None,
        since: None,
        until: None,
    })
    .unwrap()
    .into_iter()
    .map(|row| row.reason)
    .collect()
}

fn typed(id: &str, text: &str, submit: bool) -> InputParams {
    InputParams {
        id: id.to_string(),
        text: Some(text.to_string()),
        keys: None,
        submit,
        reason: WHY.to_string(),
        confirm: false,
        wake_timeout_s: 120,
    }
}

// -- the sweep (tests/test_session_sweep.py) -----------------------------------

#[test]
fn the_sweep_reads_enough_lines_to_find_an_input_box_and_shows_what_was_asked() {
    let (built, rows) = wired();
    start(&built);
    let result = sweep(&built, 1);
    // The sweep reads at least 8 lines to find an input box (WI-1870).
    assert_eq!(rows.lock().unwrap().sweep_queries, ["screen_lines=8"]);
    assert_eq!(result.rows[0].screen_tail, ["> "]);
    assert!(sweep(&built, 0).rows[0].screen_tail.is_empty());
}

#[test]
fn input_clears_a_blocked_report_and_needs_you_drops_it() {
    // WI-1834 acceptance: report blocked, inject input, no longer needs you.
    let (built, rows) = wired();
    let (worker, worker_engine) = start(&built);
    rows.lock()
        .unwrap()
        .update(&worker_engine, json!({"activity": "idle"}));
    report_blocked(&built, &worker, "Infisical returns 500");
    let before = sweep(&built, 0);
    assert_eq!(needs_you(&before), 1);
    assert_eq!(before.rows[0].attention, "blocked");
    assert!(before.rows[0].blocked_since.is_some());
    assert!(before.rows[0].blocked_age_s.is_some());

    input(
        ctx(&built),
        &typed(&worker, "Infisical is fixed, re-run", true),
    )
    .unwrap();

    assert!(!rows.lock().unwrap().inputs.is_empty());
    assert_eq!(
        rows.lock().unwrap().row(&worker_engine)["blocked"],
        Value::Null
    );
    let after = sweep(&built, 0);
    assert_eq!(needs_you(&after), 0);
    assert_ne!(after.rows[0].attention, "blocked");
    assert!(after.rows[0].blocked_since.is_none());
    // The report it replaced stays in the audit.
    assert_eq!(
        unblocked_reasons(&built, &worker),
        [format!("cleared by input: {WHY}")]
    );
}

#[test]
fn an_unsubmitted_input_does_not_clear_a_blocked_report() {
    // Python's draft: typed into the box, not sent yet.
    let (built, rows) = wired();
    let (worker, worker_engine) = start(&built);
    report_blocked(&built, &worker, "needs a go-ahead");
    input(ctx(&built), &typed(&worker, "half", false)).unwrap();
    assert_ne!(
        rows.lock().unwrap().row(&worker_engine)["blocked"],
        Value::Null
    );
}

#[test]
fn a_session_clearing_its_own_report_by_typing_is_left_alone() {
    let (built, rows) = wired();
    let (worker, worker_engine) = start(&built);
    report_blocked(&built, &worker, "needs a go-ahead");
    let own = opened_with(
        {
            let state = Arc::clone(&rows);
            Arc::new(move |url, _headers, body, method| answer(&state, url, body, method))
        },
        Principal::new(
            &format!("agent:session:{worker_engine}"),
            ActorKind::Agent,
            "Self",
        )
        .unwrap(),
    );
    input(ctx(&own), &typed(&worker_engine, "go", true)).unwrap();
    assert_ne!(
        rows.lock().unwrap().row(&worker_engine)["blocked"],
        Value::Null
    );
}

#[test]
fn a_report_older_than_the_current_turn_is_stale() {
    // Typed into straight from the terminal, or nudged on: the session went
    // back to work after reporting, so the report is stale, not needs-you.
    let (built, rows) = wired();
    let (_worker, worker_engine) = start(&built);
    let (_held, held_engine) = start(&built);
    {
        let mut rows = rows.lock().unwrap();
        rows.update(
            &worker_engine,
            json!({
                "activity": "idle",
                "turn_started_at": "2026-10-18T10:00:00Z",
                "blocked": {"reason": "ready: #197", "since": "2026-10-18T09:00:00Z"},
            }),
        );
        rows.update(
            &held_engine,
            json!({
                "activity": "idle",
                "turn_started_at": "2026-10-18T08:00:00Z",
                "blocked": {"reason": "waiting on go-ahead", "since": "2026-10-18T09:00:00Z"},
            }),
        );
    }
    let result = sweep(&built, 0);
    assert_eq!(needs_you(&result), 1);
    let held = row_of(&result, &held_engine);
    assert_eq!(held.attention, "blocked");
    assert!(!held.blocked_stale);
    let worker = row_of(&result, &worker_engine);
    assert_ne!(worker.attention, "blocked");
    assert!(worker.blocked_stale);
    assert_eq!(
        worker.blocked_since.as_deref(),
        Some("2026-10-18T09:00:00Z")
    );
}

#[test]
fn a_done_declaration_after_the_report_makes_it_stale() {
    // AUTOPILOT: DONE after the report, even in the same turn, ages it out.
    let (built, rows) = wired();
    let (_worker, worker_engine) = start(&built);
    rows.lock().unwrap().update(
        &worker_engine,
        json!({
            "activity": "idle",
            "turn_started_at": "2026-10-18T08:00:00Z",
            "done_at": "2026-10-18T09:30:00Z",
            "blocked": {"reason": "waiting on go-ahead", "since": "2026-10-18T09:00:00Z"},
        }),
    );
    let result = sweep(&built, 0);
    assert_eq!(needs_you(&result), 0);
    assert!(result.rows[0].blocked_stale);
    assert_ne!(result.rows[0].attention, "blocked");
    assert_eq!(
        result.rows[0].session.done_at.as_deref(),
        Some("2026-10-18T09:30:00Z")
    );
}

#[test]
fn a_done_session_at_its_prompt_is_done_and_ranks_after_waiting() {
    // WI-1518: done_at from the engine reads into the `done` class.
    let (built, rows) = wired();
    let (_done, done_engine) = start(&built);
    let (_waiting, waiting_engine) = start(&built);
    {
        let mut rows = rows.lock().unwrap();
        rows.update(
            &done_engine,
            json!({"activity": "waiting-for-input", "done_at": "2026-10-18T09:30:00Z"}),
        );
        rows.update(&waiting_engine, json!({"activity": "waiting-for-input"}));
    }
    let result = sweep(&built, 0);
    let order: Vec<&str> = result
        .rows
        .iter()
        .map(|row| row.attention.as_str())
        .collect();
    assert_eq!(order, ["waiting", "done"]);
    assert_eq!(
        row_of(&result, &done_engine).attention_reason,
        "said it is done (AUTOPILOT: DONE), at its prompt"
    );
    assert_eq!(result.counts["done"], json!(1));
    assert_eq!(needs_you(&result), 1);
}

#[test]
fn a_session_resting_after_its_first_turn_is_awaiting_task() {
    // WI-1854: the engine's metered turns say it only read its brief.
    let (built, rows) = wired();
    let (_fresh, fresh_engine) = start(&built);
    let (_worked, worked_engine) = start(&built);
    {
        let mut rows = rows.lock().unwrap();
        rows.update(
            &fresh_engine,
            json!({"activity": "idle", "economics": {"turns": 2}}),
        );
        rows.update(
            &worked_engine,
            json!({"activity": "idle", "economics": {"turns": 40}}),
        );
    }
    let result = sweep(&built, 0);
    let fresh = row_of(&result, &fresh_engine);
    assert_eq!(fresh.attention, "waiting");
    assert_eq!(fresh.attention_reason, crate::decisions::AWAITING_TASK);
    assert_eq!(row_of(&result, &worked_engine).attention, "idle");
}

#[test]
fn a_question_menu_is_needs_you() {
    // WI-1871: the engine reads an AskUserQuestion menu as kind=question.
    let (built, rows) = wired();
    let (_asking, asking_engine) = start(&built);
    rows.lock().unwrap().update(
        &asking_engine,
        json!({
            "activity": "awaiting-approval",
            "approval": {
                "question": "Which approach should the sweep fix take?",
                "command_excerpt": "☐ Approach\nWhich approach should the sweep fix take?",
                "kind": "question",
                "options": [
                    {"number": 1, "label": "Narrow fix", "selected": true},
                    {"number": 2, "label": "Full rewrite"},
                ],
                "detected_at": "2026-10-18T00:00:00Z",
            },
        }),
    );
    let result = sweep(&built, 0);
    assert_eq!(needs_you(&result), 1);
    let row = &result.rows[0];
    assert_eq!(row.attention, "approval");
    assert!(row.attention_reason.starts_with("asking a question"));
    let approval = row.session.approval.as_ref().unwrap();
    assert_eq!(approval.kind, "question");
    let labels: Vec<&str> = approval.options.iter().map(|o| o.label.as_str()).collect();
    assert_eq!(labels, ["Narrow fix", "Full rewrite"]);
}

#[test]
fn text_left_in_the_box_at_rest_is_input_pending() {
    // WI-1870: a session at rest with text in its box is waiting on Enter.
    let (built, rows) = wired();
    let (_held, held_engine) = start(&built);
    let (_drained, drained_engine) = start(&built);
    {
        let mut rows = rows.lock().unwrap();
        rows.update(&held_engine, json!({"activity": "idle"}));
        rows.update(&drained_engine, json!({"activity": "idle"}));
        rows.tails.insert(
            held_engine.clone(),
            claude("held_idle").lines[18..].to_vec(),
        );
        rows.tails.insert(
            drained_engine.clone(),
            claude("drained_idle").lines[18..].to_vec(),
        );
    }
    let result = sweep(&built, 0);
    let held = row_of(&result, &held_engine);
    assert_eq!(held.attention, "waiting");
    assert_eq!(held.attention_reason, crate::decisions::INPUT_PENDING);
    assert_eq!(row_of(&result, &drained_engine).attention, "idle");
    // A greyed suggestion is not input pending.
    rows.lock().unwrap().tails.insert(
        held_engine.clone(),
        boxed(&["\u{276f} Try \"fix lint errors\""]),
    );
    assert_eq!(row_of(&sweep(&built, 0), &held_engine).attention, "idle");
}

// -- wake (tests/test_session_hibernation.py) ----------------------------------

#[test]
fn waking_clears_the_blocked_report_it_slept_with() {
    // WI-1834: whoever wakes a session is acting on it; the report goes, and
    // the audit keeps it.
    let (built, rows) = wired();
    let (ses_id, engine_id) = start(&built);
    report_blocked(&built, &ses_id, "waiting on go-ahead");
    let result = wake(
        ctx(&built),
        &WakeParams {
            id: ses_id.clone(),
            reason: WHY.to_string(),
        },
    )
    .unwrap();
    assert_eq!(rows.lock().unwrap().row(&engine_id)["blocked"], Value::Null);
    assert_eq!(result.session.alive, Some(true));
    assert!(result.session.blocked.is_none());
    assert_eq!(
        unblocked_reasons(&built, &ses_id),
        [format!("cleared by wake: {WHY}")]
    );
}

// -- the task a start implies (tests/test_sessions.py, WI-1854) ----------------

fn last_prompt(rows: &Mutex<Rows>) -> String {
    rows.lock().unwrap().specs.last().unwrap()["prompt"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

fn agent_start(reason: &str) -> StartSessionParams {
    let mut params = start_params(Some("beta"), None, reason);
    params.template = Some("claude".to_string());
    params
}

#[test]
fn an_agent_start_takes_its_task_from_the_reason() {
    let (built, rows) = wired();
    start_session(
        ctx(&built),
        &agent_start("Task: fix the flaky sweep test and push"),
    )
    .unwrap();
    let brief = last_prompt(&rows);
    let task = brief.find("## Task").expect("a Task section");
    assert!(brief[task..].contains("fix the flaky sweep test and push"));
    assert!(!brief.contains("Task: fix"));
}

#[test]
fn an_explicit_task_wins_over_the_reason() {
    let (built, rows) = wired();
    let mut params = agent_start("Task: not that");
    params.task = Some("do this".to_string());
    start_session(ctx(&built), &params).unwrap();
    let brief = last_prompt(&rows);
    assert!(brief.contains("do this"));
    assert!(!brief.contains("not that"));
}

#[test]
fn a_shell_start_takes_no_task_from_its_reason() {
    let (built, rows) = wired();
    start_session(
        ctx(&built),
        &start_params(Some("beta"), None, "Task: not for a shell"),
    )
    .unwrap();
    assert!(!last_prompt(&rows).contains("## Task"));
}

#[test]
fn a_review_purpose_gets_a_default_review_task() {
    let (built, rows) = wired();
    let mut params = start_params(Some("beta"), None, WHY);
    params.purpose = Some("review".to_string());
    start_session(ctx(&built), &params).unwrap();
    let brief = last_prompt(&rows);
    let task = &brief[brief.find("## Task").expect("a Task section")..];
    assert!(task.contains("Review the pull request or branch bound to the bound work item"));
    assert!(task.contains("report blockers"));
}

// -- a Claude model runs Claude Code (tests/test_sessions.py, WI-1852) ---------

#[test]
fn a_claude_model_on_the_implement_profile_runs_claude_code() {
    // `purpose=implement, model=opus` kept the profile's klaudia template, and
    // klaudia answered 400 "Invalid model name" on every turn.
    let (built, rows) = wired();
    let mut params = start_params(Some("beta"), Some("fix it"), WHY);
    params.purpose = Some("implement".to_string());
    params.model = Some("opus".to_string());
    let result = start_session(ctx(&built), &params).unwrap();
    let spec = rows.lock().unwrap().specs.last().unwrap().clone();
    assert_eq!(spec["template"], json!("claude"));
    assert_eq!(spec["model"], json!("opus"));
    assert_eq!(result.session.template.as_deref(), Some("claude"));
    params.model = Some("claude-opus-5-5".to_string());
    start_session(ctx(&built), &params).unwrap();
    let spec = rows.lock().unwrap().specs.last().unwrap().clone();
    assert_eq!(spec["template"], json!("claude"));
    // A model that is not Claude's keeps the profile's agent.
    params.model = Some("grok-4.7".to_string());
    start_session(ctx(&built), &params).unwrap();
    let spec = rows.lock().unwrap().specs.last().unwrap().clone();
    assert_eq!(spec["template"], json!("klaudia"));
}

#[test]
fn a_claude_alias_on_another_agent_template_is_refused() {
    // A bare alias only Claude Code resolves is refused up front on klaudia,
    // codex or opencode, rather than failing every turn.
    let (built, rows) = wired();
    for template in ["klaudia", "codex", "opencode"] {
        let mut params = start_params(Some("beta"), None, WHY);
        params.template = Some(template.to_string());
        params.model = Some("opus".to_string());
        let error = start_session(ctx(&built), &params).unwrap_err();
        assert!(
            matches!(&error, VogtError::PolicyRefused(message) if message.contains("does not resolve")),
            "{error:?}"
        );
        assert_eq!(
            error.message(),
            format!(
                "template '{template}' runs {template}, which does not resolve the Claude Code \
                 alias 'opus'; use template 'claude', or name a model that agent takes"
            )
        );
    }
    let mut params = start_params(Some("beta"), None, WHY);
    params.purpose = Some("implement".to_string());
    params.template = Some("klaudia".to_string());
    params.model = Some("sonnet".to_string());
    let error = start_session(ctx(&built), &params).unwrap_err();
    assert_eq!(error.code(), "policy_refused");
    assert!(rows.lock().unwrap().specs.is_empty());
}

// -- a resumed oversight keeps its starts (tests/test_sessions.py, WI-1852) ----

const OVERSIGHT: &str = "0cc46582-ed57-4327-9a28-624a7ae2474a";

fn as_claimant(rows: &Arc<Mutex<Rows>>, actor: &str, claim: &str) -> Built {
    let state = Arc::clone(rows);
    let built = opened_with(
        Arc::new(move |url, _headers, body, method| answer(&state, url, body, method)),
        Principal::new(actor, ActorKind::Agent, "Pod").unwrap(),
    );
    insert_project(&built, "beta");
    built.with_claimed_engine_session(Some(claim))
}

fn oversight_row(rows: &Mutex<Rows>, role: &str) {
    let mut rows = rows.lock().unwrap();
    rows.rows.retain(|(id, _)| id != OVERSIGHT);
    rows.rows.push((
        OVERSIGHT.to_string(),
        json!({"id": OVERSIGHT, "name": "overseer", "activity": "idle", "alive": true,
               "cwd": "/work/tree", "role": role}),
    ));
}

#[test]
fn a_resumed_oversight_on_the_pod_token_keeps_its_starts() {
    // Oversight resumed by hand runs on the pod token, so its starts had no
    // starter. The bridge's claimed engine session counts once the engine
    // vouches it is oversight.
    let rows = Arc::new(Mutex::new(Rows::default()));
    oversight_row(&rows, "oversight");
    let pod = as_claimant(&rows, "agent:pod:vogt-prod", OVERSIGHT);
    let result = start_session(ctx(&pod), &start_params(Some("beta"), None, WHY)).unwrap();
    assert_eq!(
        result.session.started_by_session.as_deref(),
        Some(OVERSIGHT)
    );
    assert_eq!(result.session.started_by_kind.as_deref(), Some("oversight"));
}

#[test]
fn a_claimed_engine_session_is_ignored_unless_vouched_for() {
    // A worker's claim is not oversight's; only the pod token may claim.
    for (actor, role) in [
        ("agent:pod:vogt-prod", "worker"),
        ("agent:someone-else", "oversight"),
    ] {
        let rows = Arc::new(Mutex::new(Rows::default()));
        oversight_row(&rows, role);
        let built = as_claimant(&rows, actor, OVERSIGHT);
        let result = start_session(ctx(&built), &start_params(Some("beta"), None, WHY)).unwrap();
        assert_eq!(result.session.started_by_session, None, "{actor} {role}");
        assert_eq!(result.session.started_by_kind, None, "{actor} {role}");
    }
}

#[test]
fn an_unknown_claimed_engine_session_is_ignored() {
    let rows = Arc::new(Mutex::new(Rows::default()));
    let built = as_claimant(
        &rows,
        "agent:pod:vogt-prod",
        "11111111-2222-4333-8444-555555555555",
    );
    let result = start_session(ctx(&built), &start_params(Some("beta"), None, WHY)).unwrap();
    assert_eq!(result.session.started_by_session, None);
}

// -- the queued-input follow-up (tests/test_delivery.py, WI-1870) --------------

const QUEUED_ID: &str = "00000000-0000-4000-8000-000000000001";

/// `tests/test_delivery.py`'s `QueuedEngine`: a Claude session running a turn
/// as the input arrives; the turn then ends with the text left in the box.
/// Enters after the first clear it only once `takes_after` of them have been
/// pressed. The first `timeouts` waits time out.
struct Queued {
    takes_after: usize,
    timeouts: usize,
    enters: usize,
    waits: usize,
    wait_timeouts: Vec<u64>,
    held: Vec<String>,
}

impl Queued {
    fn new(takes_after: usize, timeouts: usize) -> Arc<Mutex<Self>> {
        Arc::new(Mutex::new(Self {
            takes_after,
            timeouts,
            enters: 0,
            waits: 0,
            wait_timeouts: Vec::new(),
            held: claude("held_idle").lines,
        }))
    }

    fn screen(&self) -> (&'static str, Vec<String>) {
        if self.enters == 0 {
            return ("running", claude("queued_behind_turn").lines);
        }
        if self.enters - 1 < self.takes_after {
            return ("idle", self.held.clone());
        }
        ("running", claude("drained_idle").lines)
    }
}

fn queued_transport(engine: &Arc<Mutex<Queued>>) -> Transport {
    let engine = Arc::clone(engine);
    Arc::new(move |url, _headers, body, method| {
        let mut engine = engine.lock().unwrap();
        let path = url.trim_start_matches("http://engine");
        let (path, query) = path.split_once('?').unwrap_or((path, ""));
        let (activity, lines) = engine.screen();
        let summary =
            json!({"id": QUEUED_ID, "name": "x", "activity": activity, "cwd": "/work/tree"});
        let ok = |value: Value| (200, value.to_string().into_bytes());
        if method == "POST" && path == "/api/sessions" {
            return ok(summary);
        }
        if path.ends_with("/input") {
            let asked: Value = serde_json::from_slice(body).unwrap_or(Value::Null);
            if asked["submit"] == json!(true) {
                engine.enters += 1;
            }
            return ok(json!({"ok": true}));
        }
        if path.ends_with("/wait") {
            engine.waits += 1;
            let timeout = query
                .split('&')
                .find_map(|pair| pair.strip_prefix("timeout_s="))
                .and_then(|value| value.parse().ok())
                .unwrap_or(0);
            engine.wait_timeouts.push(timeout);
            if engine.waits <= engine.timeouts {
                return ok(json!({"outcome": "timeout"}));
            }
            return ok(json!({"outcome": "ready", "matched": true}));
        }
        if path.ends_with("/screen") {
            return ok(
                json!({"id": QUEUED_ID, "lines": lines, "activity": activity, "alive": true}),
            );
        }
        if method == "GET" && path == format!("/api/sessions/{QUEUED_ID}") {
            return ok(json!({"summary": summary}));
        }
        (404, Vec::new())
    })
}

fn wired_queued(engine: &Arc<Mutex<Queued>>) -> (Built, String) {
    let built = opened_with(queued_transport(engine), person());
    insert_project(&built, "beta");
    let started = start_session(ctx(&built), &start_params(Some("beta"), None, WHY)).unwrap();
    (built, started.session.id)
}

fn send(built: &Built, id: &str, text: &str) -> super::sessions::SessionInputResult {
    let mut params = typed(id, text, true);
    params.confirm = true;
    input(ctx(built), &params).unwrap()
}

#[test]
fn a_queued_input_left_in_the_box_gets_an_enter() {
    // Once the turn ends with the text still in the box, the follow-up
    // presses Enter and the agent takes it.
    let engine = Queued::new(1, 0);
    let (built, ses_id) = wired_queued(&engine);
    let result = send(&built, &ses_id, &claude("held_idle").text);
    assert_eq!(result.delivery, "queued");
    let engine = engine.lock().unwrap();
    assert_eq!(engine.waits, 1, "it waited for the turn to end");
    assert_eq!(engine.enters, 2, "the submit, then one follow-up Enter");
}

#[test]
fn a_queued_input_the_cli_took_gets_no_extra_enter() {
    let engine = Queued::new(0, 0);
    let (built, ses_id) = wired_queued(&engine);
    send(&built, &ses_id, &claude("held_idle").text);
    assert_eq!(
        engine.lock().unwrap().enters,
        1,
        "the box was clear: no second Enter"
    );
}

#[test]
fn a_queued_input_that_will_not_submit_is_stuck() {
    let engine = Queued::new(99, 0);
    wired_queued(&engine);
    let client = EngineClient::new("http://engine", None, Some(queued_transport(&engine)));
    // The submit landed behind the turn; it has ended.
    engine.lock().unwrap().enters = 1;
    let outcome =
        super::sessions::tests_submit_queued(&client, QUEUED_ID, "x", &claude("held_idle").text)
            .unwrap();
    assert_eq!(outcome, crate::delivery::Delivery::Stuck);
    assert_eq!(engine.lock().unwrap().enters, 1 + 2, "bounded Enters");
}

#[test]
fn a_stuck_multi_line_body_gets_a_follow_up_enter() {
    let multi = "first line of the body\nsecond line\n\nthird, after a blank";
    let engine = Queued::new(1, 0);
    engine.lock().unwrap().held = boxed(&["\u{276f} [Pasted text #1 +3 lines]"]);
    let (built, ses_id) = wired_queued(&engine);
    let result = send(&built, &ses_id, multi);
    assert_eq!(result.delivery, "queued");
    assert_eq!(engine.lock().unwrap().enters, 2);
}

#[test]
fn a_long_turn_is_waited_out_in_waits_the_engine_accepts() {
    // Review of #1112: the engine refuses a wait over 600 s, so a turn longer
    // than one wait is followed in chunks, each within the cap.
    let engine = Queued::new(1, 3);
    let (built, ses_id) = wired_queued(&engine);
    send(&built, &ses_id, &claude("held_idle").text);
    let engine = engine.lock().unwrap();
    assert_eq!(engine.waits, 4, "three timed-out waits, then ready");
    assert!(engine.wait_timeouts.iter().all(|&timeout| timeout <= 600));
    assert_eq!(engine.enters, 2, "the follow-up Enter still came");
}

#[test]
fn a_turn_longer_than_the_follow_up_gets_no_enter() {
    let engine = Queued::new(1, 1000);
    let (built, ses_id) = wired_queued(&engine);
    send(&built, &ses_id, &claude("held_idle").text);
    let engine = engine.lock().unwrap();
    assert!(engine.waits as u64 * 600 >= 3600);
    assert!(engine.wait_timeouts.iter().all(|&timeout| timeout <= 600));
    assert_eq!(engine.enters, 1);
}

#[test]
fn what_the_follow_up_cannot_absorb_is_an_error_it_logs() {
    // Python's `_submit_queued_logged` logs what `_submit_queued` raises; the
    // Rust follow-up returns it to the logging wrapper instead of panicking.
    let transport: Transport =
        Arc::new(|_url, _headers, _body, _method| (403, br#"{"error":"forbidden: no"}"#.to_vec()));
    let client = EngineClient::new("http://engine", None, Some(transport));
    let error = super::sessions::tests_submit_queued(&client, QUEUED_ID, "x", "text").unwrap_err();
    assert_eq!(error.code(), "grant_refused");
}
