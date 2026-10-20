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
    /// What the list says it hid (WI-1289).
    hidden_count: u64,
    /// The query string of every `GET /api/sessions`.
    list_queries: Vec<String>,
    /// The screen lines a wait answers with.
    wait_lines: Vec<String>,
    /// How many `GET /api/sessions/{id}` reads were made.
    gets: usize,
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
        rows.list_queries.push(query);
        let listed: Vec<Value> = rows.rows.iter().map(|(_, row)| row.clone()).collect();
        return ok(&json!({"sessions": listed, "hidden_count": rows.hidden_count}));
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
            rows.gets += 1;
            return ok(rows.row(&id));
        }
        if method == "GET" && tail == "wait" {
            let row = rows.row(&id).clone();
            return ok(&json!({
                "outcome": "awaiting-approval",
                "matched": false,
                "waited_ms": 1234,
                "screen": {
                    "id": id,
                    "cols": 80,
                    "rows": 24,
                    "lines": rows.wait_lines,
                    "activity": row["activity"],
                    "alive": true,
                    "ready": false,
                },
            }));
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
        // Python's sweep tests read `row.session`, so they sweep `mode="full"`.
        &SweepSessionsParams {
            mode: "full".to_string(),
            screen_lines,
            ..SweepSessionsParams::default()
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
        .find(|row| session_of(row).engine_session_id == engine_id)
        .expect("a row for the session")
}

/// A full-mode row's summary.
fn session_of(row: &super::sessions::SessionSweepRow) -> &super::sessions::SessionSummary {
    row.session
        .as_ref()
        .expect("a full-mode row carries the summary")
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
        session_of(&result.rows[0]).done_at.as_deref(),
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
    let approval = session_of(row).approval.as_ref().unwrap();
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

// -- digest rows (design C1): tests/test_session_sweep.py and test_sessions.py

use super::sessions::{list_sessions, wait, ListSessionsParams, SessionDigest, WaitParams};

fn swept(built: &Built, params: SweepSessionsParams) -> SessionSweepResult {
    sweep_sessions(ctx(built), &params).unwrap()
}

fn listed(built: &Built, params: ListSessionsParams) -> super::sessions::SessionListResult {
    list_sessions(ctx(built), &params).unwrap()
}

fn full(limit: i64, offset: i64) -> ListSessionsParams {
    ListSessionsParams {
        mode: "full".to_string(),
        limit,
        offset,
        ..ListSessionsParams::default()
    }
}

/// Pydantic's `model_dump_json(exclude_none=True)`: compact, nulls dropped.
fn without_nulls(value: Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.into_iter()
                .filter(|(_, value)| !value.is_null())
                .map(|(key, value)| (key, without_nulls(value)))
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.into_iter().map(without_nulls).collect()),
        other => other,
    }
}

/// `SessionDigest.over_budget`: its JSON, nulls dropped, over 300 characters.
fn over_budget(digest: &SessionDigest) -> bool {
    let text = without_nulls(serde_json::to_value(digest).unwrap()).to_string();
    text.chars().count() > 300
}

fn digest_of_row(row: &super::sessions::SessionSweepRow) -> &SessionDigest {
    row.digest.as_ref().expect("a digest row")
}

/// An engine session Vogt never linked.
fn unlinked(rows: &Mutex<Rows>, id: &str, activity: &str) {
    rows.lock().unwrap().rows.push((
        id.to_string(),
        json!({"id": id, "name": id, "activity": activity, "alive": true, "cwd": "/work/tree"}),
    ));
}

fn hibernated(rows: &Mutex<Rows>, engine_id: &str) {
    rows.lock()
        .unwrap()
        .update(engine_id, json!({"activity": "hibernated", "alive": false}));
}

#[test]
fn the_default_sweep_is_a_digest_that_drops_hibernated_and_keeps_counts() {
    // C1: the default sweep is a digest. Counts include the rows the filter drops.
    let (built, rows) = wired();
    let started: Vec<(String, String)> = (0..3).map(|_| start(&built)).collect();
    let (_, asleep) = start(&built);
    hibernated(&rows, &asleep);

    let result = swept(&built, SweepSessionsParams::default());

    assert_eq!(result.counts["total"], json!(4));
    assert_eq!(result.counts["hibernated"], json!(1));
    assert_eq!(result.rows.len(), 3);
    let mut ids: Vec<&str> = result
        .rows
        .iter()
        .map(|row| digest_of_row(row).id.as_str())
        .collect();
    ids.sort_unstable();
    let mut live: Vec<&str> = started.iter().map(|(id, _)| id.as_str()).collect();
    live.sort_unstable();
    assert_eq!(ids, live);
    assert!(result.rows.iter().all(|row| row.session.is_none()));
    assert!(result.rows.iter().all(|row| row.screen_tail.is_empty()));
    let payload = without_nulls(serde_json::to_value(&result).unwrap()).to_string();
    assert!(payload.len() < 15_000);
    for row in &result.rows {
        assert!(!over_budget(digest_of_row(row)), "{:?}", row.digest);
        // No economics reported yet, so the digest carries no budget line.
        assert_eq!(digest_of_row(row).budget, None);
    }

    let (metered, metered_engine) = &started[0];
    rows.lock().unwrap().update(
        metered_engine,
        json!({"economics": {"turns": 7, "compactions": 2}}),
    );
    let again = swept(&built, SweepSessionsParams::default());
    for row in &again.rows {
        let digest = digest_of_row(row);
        if &digest.id == metered {
            assert_eq!(digest.budget.as_deref(), Some("7 turns, 2 compactions"));
        } else {
            assert_eq!(digest.budget, None);
        }
    }
}

#[test]
fn a_digest_keeps_the_full_blocked_reason() {
    // WI-1670: the head stays, and the guard's full reason rides along.
    let (built, rows) = wired();
    let (blocked, blocked_engine) = start(&built);
    let reason = format!("the commit guard refuses git add: {}", "x".repeat(200));
    rows.lock().unwrap().update(
        &blocked_engine,
        json!({
            "activity": "waiting-for-input",
            "blocked": {"reason": reason, "items": [], "since": "2026-10-05T00:00:00Z"},
        }),
    );
    let result = swept(&built, SweepSessionsParams::default());
    let digest = result
        .rows
        .iter()
        .map(digest_of_row)
        .find(|digest| digest.id == blocked)
        .unwrap();
    assert_eq!(
        digest.blocked_reason.as_deref(),
        Some(&reason[..120]),
        "the first 120 characters"
    );
    assert_eq!(digest.blocked_detail.as_deref(), Some(reason.as_str()));

    let (short, short_engine) = start(&built);
    rows.lock().unwrap().update(
        &short_engine,
        json!({
            "activity": "waiting-for-input",
            "blocked": {"reason": "short reason", "items": [], "since": "2026-10-05T00:00:00Z"},
        }),
    );
    let again = swept(&built, SweepSessionsParams::default());
    let digest = again
        .rows
        .iter()
        .map(digest_of_row)
        .find(|digest| digest.id == short)
        .unwrap();
    assert_eq!(digest.blocked_reason.as_deref(), Some("short reason"));
    assert_eq!(digest.blocked_detail, None);
}

#[test]
fn a_digest_clips_by_character_not_byte() {
    // Python slices code points: a 200-character report of two-byte
    // characters keeps 120 of them, not 120 bytes.
    let (built, rows) = wired();
    let (_, engine_id) = start(&built);
    let reason = "é".repeat(200);
    rows.lock().unwrap().update(
        &engine_id,
        json!({"activity": "idle", "blocked": {"reason": reason, "items": [], "since": null}}),
    );
    let result = swept(&built, SweepSessionsParams::default());
    let digest = digest_of_row(&result.rows[0]);
    assert_eq!(
        digest.blocked_reason.as_deref(),
        Some("é".repeat(120).as_str())
    );
    assert_eq!(digest.blocked_detail.as_deref(), Some(reason.as_str()));
}

#[test]
fn the_digest_sweep_filters_by_attention_and_limit() {
    let (built, rows) = wired();
    let (asking, asking_engine) = start(&built);
    start(&built);
    rows.lock().unwrap().update(
        &asking_engine,
        json!({
            "activity": "awaiting-approval",
            "approval": {"question": "Proceed?", "detected_at": "2026-10-05T00:00:00Z"},
        }),
    );
    let result = swept(
        &built,
        SweepSessionsParams {
            attention: Some(vec!["needs_you".to_string()]),
            limit: 1,
            ..SweepSessionsParams::default()
        },
    );
    assert_eq!(result.rows.len(), 1);
    assert_eq!(digest_of_row(&result.rows[0]).id, asking);
    assert_eq!(result.counts["total"], json!(2));
    assert_eq!(result.counts["needs_you"], json!(1));
}

#[test]
fn a_question_menu_digest_carries_its_question_and_options() {
    // WI-1871: oversight answers from the digest with session_answer.
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
    let result = swept(
        &built,
        SweepSessionsParams {
            attention: Some(vec!["needs_you".to_string()]),
            ..SweepSessionsParams::default()
        },
    );
    assert_eq!(result.counts["needs_you"], json!(1));
    assert_eq!(result.rows.len(), 1);
    let row = &result.rows[0];
    assert_eq!(row.attention, "approval");
    assert!(row.attention_reason.starts_with("asking a question"));
    let digest = digest_of_row(row);
    assert_eq!(
        digest.question.as_deref(),
        Some("Which approach should the sweep fix take?")
    );
    assert_eq!(digest.question_kind.as_deref(), Some("question"));
    assert_eq!(
        digest.options.as_deref(),
        Some(&["Narrow fix".to_string(), "Full rewrite".to_string()][..])
    );
}

#[test]
fn a_done_row_is_kept_by_the_default_digest_with_its_done_at() {
    // WI-1835 keeps `done` in the default digest states; WI-1518's done_at
    // rides on the digest row.
    let (built, rows) = wired();
    let (done, done_engine) = start(&built);
    rows.lock().unwrap().update(
        &done_engine,
        json!({"activity": "waiting-for-input", "done_at": "2026-10-18T09:30:00Z"}),
    );
    let result = swept(&built, SweepSessionsParams::default());
    let row = &result.rows[0];
    assert_eq!(row.attention, "done");
    let digest = digest_of_row(row);
    assert_eq!(digest.id, done);
    assert_eq!(digest.done_at.as_deref(), Some("2026-10-18T09:30:00Z"));
    assert_eq!(digest.attention.as_deref(), Some("done"));
    assert_eq!(digest.ready, Some(true));
}

#[test]
fn an_empty_state_list_keeps_every_state() {
    let (built, rows) = wired();
    let (_, asleep) = start(&built);
    hibernated(&rows, &asleep);
    let named = |states: Option<Vec<String>>| {
        swept(
            &built,
            SweepSessionsParams {
                states,
                ..SweepSessionsParams::default()
            },
        )
        .rows
        .len()
    };
    assert_eq!(named(None), 0);
    assert_eq!(named(Some(Vec::new())), 1);
    assert_eq!(named(Some(vec!["hibernated".to_string()])), 1);
    assert_eq!(named(Some(vec!["running".to_string()])), 0);
}

#[test]
fn unlinked_sessions_are_not_repeated_across_pages() {
    let (built, rows) = wired();
    unlinked(&rows, "eng-foreign", "running");
    let first = listed(&built, full(50, 0));
    let later = listed(&built, full(50, 50));
    assert!(first.sessions.iter().any(|row| row.id == "eng-foreign"));
    assert!(!later.sessions.iter().any(|row| row.id == "eng-foreign"));
    assert_eq!((first.total, first.truncated), (1, false));
    assert_eq!(later.total, 1);
    assert!(later.sessions.is_empty());
}

#[test]
fn a_page_of_hibernated_links_does_not_hide_a_live_unlinked_session() {
    // WI-1266: filter, then page.
    let (built, rows) = wired();
    let mut first = None;
    for _ in 0..51 {
        let (id, engine_id) = start(&built);
        first.get_or_insert(id);
        hibernated(&rows, &engine_id);
    }
    unlinked(&rows, "eng-live", "running");

    let digest = listed(&built, ListSessionsParams::default());
    let ids: Vec<&str> = digest.digests.iter().map(|row| row.id.as_str()).collect();
    assert_eq!(ids, ["eng-live"]);
    assert_eq!((digest.total, digest.truncated), (1, false));
    assert!(digest.sessions.is_empty());

    // Full mode does not apply the digest state filter, so the hibernated
    // links lead and the live unlinked session is on the next page.
    let whole = listed(&built, full(50, 0));
    assert!(!whole.sessions.iter().any(|row| row.id == "eng-live"));
    assert_eq!((whole.total, whole.truncated), (52, true));
    assert_eq!(whole.sessions.len(), 50);

    let page = listed(&built, full(50, 50));
    let ids: Vec<&str> = page.sessions.iter().map(|row| row.id.as_str()).collect();
    assert_eq!(ids, [first.unwrap().as_str(), "eng-live"]);
    assert_eq!((page.total, page.truncated), (52, false));
}

#[test]
fn more_declared_links_than_the_read_cap_is_reported_truncated() {
    // WI-1266 review: one past the 500 links that are read keeps `truncated`
    // true and `total` a floor, on the last page and on an empty one past it.
    let (built, _rows) = wired();
    let mut oldest = None;
    for _ in 0..501 {
        let (_, engine_id) = start(&built);
        oldest.get_or_insert(engine_id);
    }
    let every = |offset| ListSessionsParams {
        states: Some(Vec::new()),
        ..full(500, offset)
    };
    let last = listed(&built, every(0));
    assert_eq!(last.sessions.len(), 500);
    assert_eq!((last.truncated, last.total), (true, 501));
    let beyond = listed(&built, every(500));
    assert!(beyond.sessions.is_empty());
    assert_eq!((beyond.truncated, beyond.total), (true, 501));
    let oldest = oldest.unwrap();
    assert!(!last
        .sessions
        .iter()
        .any(|row| row.engine_session_id == oldest));
}

#[test]
fn the_engines_hidden_count_is_reported_and_the_flag_is_forwarded() {
    // WI-1289: the rail's "N older sessions hidden" reads the engine's count,
    // and Show asks for the rows it left out.
    let (built, rows) = wired();
    rows.lock().unwrap().hidden_count = 4;
    start(&built);
    let plain = listed(&built, full(50, 0));
    assert_eq!(plain.sessions.len(), 1);
    assert_eq!(plain.hidden_count, 4);
    assert!(rows
        .lock()
        .unwrap()
        .list_queries
        .iter()
        .all(|query| !query.contains("include=hidden")));
    let shown = listed(
        &built,
        ListSessionsParams {
            include_hidden: true,
            ..full(50, 0)
        },
    );
    assert_eq!(shown.hidden_count, 4);
    assert_eq!(
        rows.lock().unwrap().list_queries.last().map(String::as_str),
        Some("include=hidden")
    );
}

#[test]
fn the_list_filters_by_who_started_it() {
    // WI-1672: by the starter's engine id or its `ses_…` id; unlinked engine
    // sessions (no starter) never match.
    let rows = Arc::new(Mutex::new(Rows::default()));
    let pod = as_claimant(&rows, "agent:pod:vogt-prod", "not-a-claim");
    let parent = start_session(ctx(&pod), &start_params(Some("beta"), None, WHY)).unwrap();
    let parent_engine = parent.session.engine_session_id.clone();
    rows.lock()
        .unwrap()
        .update(&parent_engine, json!({"role": "oversight"}));
    unlinked(&rows, "eng-foreign", "running");
    let pod = pod.with_claimed_engine_session(Some(&parent_engine));
    let child = start_session(ctx(&pod), &start_params(Some("beta"), None, WHY)).unwrap();
    assert_eq!(
        child.session.started_by_session.as_deref(),
        Some(parent_engine.as_str())
    );
    for named in [parent_engine.clone(), parent.session.id.clone()] {
        let owned = listed(
            &pod,
            ListSessionsParams {
                started_by_session: Some(named),
                ..full(50, 0)
            },
        );
        let ids: Vec<&str> = owned.sessions.iter().map(|row| row.id.as_str()).collect();
        assert_eq!(ids, [child.session.id.as_str()]);
    }
}

#[test]
fn a_digest_list_row_carries_attention_ready_budget_and_the_fields_asked_for() {
    let (built, rows) = wired();
    let (id, engine_id) = start(&built);
    rows.lock().unwrap().update(
        &engine_id,
        json!({"activity": "waiting-for-input", "economics": {"turns": 3}}),
    );
    let result = listed(
        &built,
        ListSessionsParams {
            fields: vec!["cwd".into(), "command".into(), "no_such_field".into()],
            ..ListSessionsParams::default()
        },
    );
    assert!(result.sessions.is_empty());
    assert_eq!(result.total, 1);
    let digest = &result.digests[0];
    assert_eq!(digest.id, id);
    assert_eq!(digest.attention.as_deref(), Some("waiting"));
    assert_eq!(digest.ready, Some(true));
    assert_eq!(digest.budget.as_deref(), Some("3 turns"));
    // `command` stays off the digest; an unknown name is skipped.
    assert_eq!(
        serde_json::to_value(&digest.extra).unwrap(),
        json!({"cwd": "/work/tree"})
    );
    // Without fields, `extra` is null rather than an empty object.
    let plain = listed(&built, ListSessionsParams::default());
    assert_eq!(plain.digests[0].extra, None);
}

#[test]
fn start_hibernate_and_wake_carry_the_digest_row() {
    // C1: the summary stays and the digest row rides beside it. The budget
    // line is left to the economics tailer (WI-1245 A2), not built here.
    let (built, _rows) = wired();
    let started = start_session(ctx(&built), &start_params(Some("beta"), None, WHY)).unwrap();
    let digest = started.digest.as_ref().expect("start carries the digest");
    assert_eq!(digest.id, started.session.id);
    assert_eq!(digest.activity.as_deref(), Some("running"));
    assert_eq!(digest.attention, None);
    assert_eq!(digest.ready, None);
    assert_eq!(digest.budget, None);
    let woken = wake(
        ctx(&built),
        &WakeParams {
            id: started.session.id.clone(),
            reason: WHY.to_string(),
        },
    )
    .unwrap();
    assert_eq!(woken.digest.as_ref().unwrap().id, started.session.id);
    let value = serde_json::to_value(&woken).unwrap();
    let keys: Vec<&str> = value
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(keys, ["session", "digest"]);
}

#[test]
fn input_carries_the_digest_of_the_session_typed_into() {
    let (built, rows) = wired();
    let (id, _engine_id) = start(&built);
    let before = rows.lock().unwrap().gets;
    let sent = input(ctx(&built), &typed(&id, "half", false)).unwrap();
    // A draft reads the session once more, for the digest row.
    assert_eq!(rows.lock().unwrap().gets, before + 1);
    assert_eq!(sent.digest.as_ref().unwrap().id, id);
}

#[test]
fn wait_returns_the_digest_and_a_tail_unless_the_screen_is_asked_for() {
    // WI-874 with C1: `screen=true` restores the full screen; the default is
    // the digest row plus the last 8 non-blank lines.
    let (built, rows) = wired();
    let (id, engine_id) = start(&built);
    rows.lock().unwrap().wait_lines = (1..=15)
        .map(|n| {
            if n % 3 == 0 {
                String::new()
            } else {
                format!("line {n}")
            }
        })
        .collect();
    let asked = |screen| WaitParams {
        id: id.clone(),
        until: "any_change".to_string(),
        timeout_s: 30,
        screen,
    };
    let waited = wait(ctx(&built), &asked(true)).unwrap();
    assert_eq!(waited.engine_session_id, engine_id);
    assert_eq!(waited.outcome, "awaiting-approval");
    assert!(!waited.matched);
    assert_eq!(waited.waited_ms, 1234);
    assert_eq!(waited.screen.as_ref().unwrap().ready, Some(false));
    assert!(waited.digest.is_none());
    assert!(waited.screen_tail.is_empty());

    let digested = wait(ctx(&built), &asked(false)).unwrap();
    assert!(digested.screen.is_none());
    assert_eq!(digested.digest.as_ref().unwrap().id, id);
    assert_eq!(
        digested.screen_tail,
        ["line 4", "line 5", "line 7", "line 8", "line 10", "line 11", "line 13", "line 14"]
    );
}
