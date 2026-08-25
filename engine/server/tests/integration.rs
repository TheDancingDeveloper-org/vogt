//! End-to-end integration tests: start the real Axum server on an OS-assigned
//! port, talk to it over HTTP + WebSocket the same way a client would.

use std::{
    os::unix::fs::PermissionsExt,
    sync::{Arc, Mutex},
    time::Duration,
};

use base64::Engine as _;
use futures_util::{SinkExt, StreamExt};
use reqwest::StatusCode;
use serde_json::{json, Value};
use time::OffsetDateTime;
use tokio_tungstenite::tungstenite::Message;
use vogt_engine_contract::{ServerEvent, SessionDetail};
use vogt_engine_server::{app::router, app::AppState, config::SessionTemplate, Config};

const TEST_TOKEN: &str = "test-token-1234567890abcdef";

fn test_config() -> Config {
    Config {
        bind: "127.0.0.1:0".parse().unwrap(),
        token: TEST_TOKEN.to_string(),
        token_mutating_request_limit_per_minute: 600,
        extra_tokens: vec![],
        scrollback_bytes: 64 * 1024,
        default_shell: "/bin/bash".to_string(),
        default_cwd: std::env::temp_dir(),
        activity_idle_after_ms: 200,
        idle_stall_after_ms: 10 * 60 * 1_000,
        workspace_root: std::env::temp_dir(),
        gui_stream_url: None,
        gui_stream_verified: false,
        state_dir: tempfile::tempdir().unwrap().keep(),
        fcm_service_account_json: None,
        vapid_subject: "mailto:test@example.invalid".to_string(),
        allowed_origins: vec![],
        auto_agent_auth: false,
        agent_auth_helper: "/usr/local/bin/mydevenv2-agent-auth".into(),
        // The synthetic agent CLI is registered as a session preset in the
        // *test* config — never the production defaults — so agent-task
        // scenarios can be driven without a real `claude`/`codex` in a PTY
        // (#296). A run selects it by taking the preset's command, exactly as
        // it would a real agent template.
        session_templates: vec![fake_agent_template()],
        assistant_api_key: None,
        assistant_base_url: "https://api.example.com/v1".into(),
        assistant_model: "gpt-5.4-mini".into(),
        assistant_max_tool_calls: 8,
        assistant_allow_claude_proxy: false,
        assistant_reasoning_effort: None,
        assistant_profiles: vec![],
        assistant_default_profile: None,
        assistant_log_retention_days: 30,
        assistant_stt_base_urls: vec![],
        assistant_stt_api_key: None,
        assistant_stt_model: "whisper-1".into(),
        assistant_tts_base_urls: vec![],
        assistant_tts_api_key: None,
        assistant_tts_model: "tts-1".into(),
        assistant_tts_voice: "alloy".into(),
        assistant_speech_attempt_timeout_ms: 30_000,
        public_url: None,
        vogt_core_url: None,
        vogt_import_root: None,
        vogt_engine_state_dir: None,
        vogt_core_token: None,
    }
}

async fn boot() -> (String, tokio::task::JoinHandle<()>) {
    boot_with_config(test_config()).await
}

async fn boot_with_config(cfg: Config) -> (String, tokio::task::JoinHandle<()>) {
    let (base, _state, handle) = boot_with_state(cfg).await;
    (base, handle)
}

/// Boot and keep the `AppState`, so a test can publish onto the engine's own
/// event bus — the same bus `vogt_core::spawn_event_follower` republishes
/// vogt-core's events onto — and drive the agent-task trigger watcher (#290)
/// with synthetic core events, without needing a real vogt-core beside it.
async fn boot_with_state(cfg: Config) -> (String, Arc<AppState>, tokio::task::JoinHandle<()>) {
    let (router, state) = router(cfg).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    (format!("http://{addr}"), state, handle)
}

/// A synthetic vogt-core event on the engine bus, the shape the follower
/// publishes (#290) — used to drive the trigger watcher in tests.
fn publish_core_event(state: &AppState, kind: &str, entity_id: &str, seq: i64, summary: Value) {
    state.bus.publish(ServerEvent::VogtChanged {
        kind: kind.to_string(),
        entity_kind: "work_item".to_string(),
        entity_id: entity_id.to_string(),
        seq,
        summary,
    });
}

/// Poll a task until its run list has at least `n` runs, or time out.
async fn wait_for_run_count(
    client: &reqwest::Client,
    base: &str,
    task_id: &str,
    n: usize,
) -> Value {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            tokio::time::sleep(Duration::from_millis(40)).await;
            let detail: Value = client
                .get(format!("{base}/api/agent-tasks/{task_id}"))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            if detail["runs"].as_array().map(|r| r.len()).unwrap_or(0) >= n {
                break detail;
            }
        }
    })
    .await
    .expect("the task should reach the expected run count")
}

fn auth() -> reqwest::header::HeaderMap {
    auth_for(TEST_TOKEN)
}

fn auth_for(token: &str) -> reqwest::header::HeaderMap {
    let mut h = reqwest::header::HeaderMap::new();
    h.insert(
        reqwest::header::AUTHORIZATION,
        format!("Bearer {token}").parse().unwrap(),
    );
    h
}

/// Absolute path to `scripts/fake-agent`, the synthetic agent CLI stand-in
/// (#296). Resolved from the crate manifest so the test does not care what the
/// working directory is when it runs.
fn fake_agent_path() -> String {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../scripts/fake-agent")
        .canonicalize()
        .expect("scripts/fake-agent must exist next to the engine")
        .to_string_lossy()
        .into_owned()
}

/// The fake-agent registered as a session preset, the way a deployment
/// registers a real agent CLI (`Claude Code (protected)` and friends). A task
/// run then reaches it through the preset's `command`.
fn fake_agent_template() -> SessionTemplate {
    SessionTemplate {
        name: "Fake Agent (test)".to_string(),
        description: "Deterministic synthetic agent CLI for agent-task tests (#296)".to_string(),
        command: Some(vec![fake_agent_path()]),
        cwd: None,
        env: vec![],
        default_name: Some("fake-agent-{timestamp}".to_string()),
        match_repo_names: vec![],
        match_path_prefixes: vec![],
        tags: vec!["agent".to_string(), "test".to_string()],
    }
}

/// The command a run uses to invoke the fake-agent preset with a scenario.
fn fake_agent_command(scenario: &str) -> Vec<String> {
    vec![fake_agent_path(), scenario.to_string()]
}

/// Poll a task until its latest run leaves `running`, or time out.
async fn wait_for_run_finish(client: &reqwest::Client, base: &str, task_id: &str) -> Value {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            tokio::time::sleep(Duration::from_millis(40)).await;
            let detail: Value = client
                .get(format!("{base}/api/agent-tasks/{task_id}"))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            if detail["runs"][0]["status"] != "running" {
                break detail;
            }
        }
    })
    .await
    .expect("the run should reach a terminal status")
}

#[tokio::test]
async fn healthz_is_public() {
    let (base, _h) = boot().await;
    let res = reqwest::get(format!("{base}/healthz")).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body: Value = res.json().await.unwrap();
    assert_eq!(body["ok"], true);
}

#[tokio::test]
async fn readyz_is_public_and_returns_checks() {
    let (base, _h) = boot().await;
    let res = reqwest::get(format!("{base}/readyz")).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body: Value = res.json().await.unwrap();
    assert_eq!(body["ok"], true);
    let checks = body["checks"].as_array().expect("missing checks");
    assert!(!checks.is_empty(), "expected readiness checks");
    assert!(checks.iter().any(|check| check["name"] == "workspace_root"));
    assert!(checks.iter().any(|check| check["name"] == "state_dir"));
}

#[tokio::test]
async fn readyz_fails_when_workspace_root_disappears() {
    let workspace = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    let mut cfg = test_config();
    cfg.default_cwd = workspace.path().to_path_buf();
    cfg.workspace_root = workspace.path().to_path_buf();
    cfg.state_dir = state_dir.path().to_path_buf();

    let (base, _h) = boot_with_config(cfg).await;
    workspace.close().unwrap();

    let res = reqwest::get(format!("{base}/readyz")).await.unwrap();
    assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body: Value = res.json().await.unwrap();
    assert_eq!(body["ok"], false);
    let checks = body["checks"].as_array().expect("missing checks");
    let workspace_check = checks
        .iter()
        .find(|check| check["name"] == "workspace_root")
        .expect("missing workspace_root check");
    assert_eq!(workspace_check["ok"], false);
}

#[tokio::test]
async fn config_endpoint_is_public_and_returns_shape() {
    let (base, _h) = boot().await;
    let res = reqwest::get(format!("{base}/api/config")).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body: Value = res.json().await.unwrap();
    assert!(
        body.get("gui_stream_url").is_some(),
        "missing gui_stream_url"
    );
    assert_eq!(
        body["gui_stream_available"], false,
        "unverified test config must withdraw the GUI surface"
    );
    assert!(body["version"].as_str().is_some(), "missing version");
}

#[tokio::test]
async fn status_endpoint_requires_auth_and_returns_shape() {
    let (base, _h) = boot().await;

    let unauth = reqwest::get(format!("{base}/api/status")).await.unwrap();
    assert_eq!(unauth.status(), StatusCode::UNAUTHORIZED);

    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let body: Value = client
        .get(format!("{base}/api/status"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert!(body["version"].as_str().is_some(), "missing version");
    assert!(
        body["session_count"].as_u64().is_some(),
        "missing session_count"
    );
    assert!(
        body["push_subscription_count"].as_u64().is_some(),
        "missing push_subscription_count"
    );
    assert!(
        body["gui_process_count"].as_u64().is_some(),
        "missing gui_process_count"
    );
    assert!(
        body["auth_broker"]["auto_agent_auth"].is_boolean(),
        "missing auth_broker.auto_agent_auth"
    );
    assert!(
        body["storage"]["workspace_root"].as_str().is_some(),
        "missing storage.workspace_root"
    );
}

#[tokio::test]
async fn push_subscribe_list_unsubscribe() {
    let (base, _h) = boot().await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    // VAPID public key is reachable without auth and non-empty.
    let pk: Value = reqwest::get(format!("{base}/api/push/public-key"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(!pk["vapid_public_key"].as_str().unwrap_or("").is_empty());

    // Subscribe a fake FCM token.
    let r: Value = client
        .post(format!("{base}/api/push/subscribe"))
        .json(&json!({
            "kind": "fcm",
            "token": "fake-test-token-12345",
            "label": "test-device",
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = r["id"].as_str().unwrap().to_string();

    let list: Vec<Value> = client
        .get(format!("{base}/api/push/list"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(list.iter().any(|s| s["id"] == id));

    // Re-subscribing the same token is idempotent (same id).
    let r2: Value = client
        .post(format!("{base}/api/push/subscribe"))
        .json(&json!({"kind":"fcm","token":"fake-test-token-12345"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(r2["id"], r["id"]);

    let r: Value = client
        .post(format!("{base}/api/push/unsubscribe"))
        .json(&json!({"id": id}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(r["ok"], true);
}

#[tokio::test]
async fn push_preferences_and_quiet_hour_digest_queue_are_exposed() {
    let (base, _h) = boot().await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    let r: Value = client
        .post(format!("{base}/api/push/subscribe"))
        .json(&json!({
            "kind": "fcm",
            "token": "fake-digest-token-12345",
            "label": "quiet-device",
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = r["id"].as_str().unwrap().to_string();

    let now = OffsetDateTime::now_utc();
    let minute = (u16::from(now.hour()) * 60) + u16::from(now.minute());
    let start = minute.saturating_sub(1);
    let end = (minute + 2) % (24 * 60);

    let updated: Value = client
        .post(format!("{base}/api/push/update"))
        .json(&json!({
            "id": id,
            "prefs": {
                "waiting_for_input": true,
                "agent_task_started": false,
                "agent_task_notify": false,
                "quiet_hours": {
                    "enabled": true,
                    "start_minute": start,
                    "end_minute": end,
                    "utc_offset_minutes": 0,
                    "digest": true
                }
            }
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(updated["ok"], true);
    assert_eq!(updated["prefs"]["quiet_hours"]["enabled"], true);

    let queued: Value = client
        .post(format!("{base}/api/push/test"))
        .json(&json!({"title": "Queued test"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(queued["ok"], 0);
    assert_eq!(queued["fail"], 0);
    assert_eq!(queued["queued"], 1);

    let list: Vec<Value> = client
        .get(format!("{base}/api/push/list"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let entry = list
        .iter()
        .find(|sub| sub["id"] == id)
        .expect("subscription listed");
    assert_eq!(entry["pending_digest_count"], 1);

    let flush: Value = client
        .post(format!("{base}/api/push/flush-digests"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(flush["ok"], 0);
    assert_eq!(flush["fail"], 0);

    let _updated: Value = client
        .post(format!("{base}/api/push/update"))
        .json(&json!({
            "id": id,
            "prefs": {
                "waiting_for_input": true,
                "agent_task_started": false,
                "agent_task_notify": false,
                "quiet_hours": {
                    "enabled": false,
                    "start_minute": start,
                    "end_minute": end,
                    "utc_offset_minutes": 0,
                    "digest": true
                }
            }
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let flush_after_disable: Value = client
        .post(format!("{base}/api/push/flush-digests"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(flush_after_disable["ok"], 0);
    assert_eq!(flush_after_disable["fail"], 1);

    let r: Value = client
        .post(format!("{base}/api/push/unsubscribe"))
        .json(&json!({"id": id}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(r["ok"], true);
}

#[tokio::test]
async fn mutating_requests_are_rate_limited_per_token() {
    let mut cfg = test_config();
    cfg.token_mutating_request_limit_per_minute = 2;

    let (base, _h) = boot_with_config(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    for name in ["one", "two"] {
        let res = client
            .post(format!("{base}/api/sessions"))
            .json(&json!({ "name": name, "command": ["/bin/true"] }))
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
    }

    let limited = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({ "name": "three", "command": ["/bin/true"] }))
        .send()
        .await
        .unwrap();
    assert_eq!(limited.status(), StatusCode::TOO_MANY_REQUESTS);
    assert!(limited.headers().get("retry-after").is_some());
}

#[tokio::test]
async fn agent_task_create_run_and_records_prompt_file() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = test_config();
    cfg.default_cwd = tmp.path().to_path_buf();
    cfg.workspace_root = tmp.path().canonicalize().unwrap();
    cfg.state_dir = tmp.path().join("state");

    let (base, _h) = boot_with_config(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    let created: Value = client
        .post(format!("{base}/api/agent-tasks"))
        .json(&json!({
            "name": "PX3 price monitor",
            "prompt": "Check Australian Hisense PX3 prices and notify only on a price drop.",
            "schedule": { "kind": "manual" },
            "command": ["/bin/sh", "-lc", "printf 'task:%s run:%s\\n' \"$MYDEVENV2_AGENT_TASK_ID\" \"$MYDEVENV2_AGENT_TASK_RUN_ID\""],
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let task_id = created["id"].as_str().unwrap().to_string();
    assert_eq!(created["name"], "PX3 price monitor");
    // The default notify prefix was renamed to `VOGT_NOTIFY:` (#203); the
    // engine still recognises the legacy `MYDEVENV2_NOTIFY:` at match time.
    assert_eq!(created["notify_on_phrase"], "VOGT_NOTIFY:");

    let run: Value = client
        .post(format!("{base}/api/agent-tasks/{task_id}/run"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let prompt_file = run["prompt_file"].as_str().unwrap();
    let session_id = run["session_id"].as_str().unwrap().to_string();
    let prompt_text = std::fs::read_to_string(prompt_file).unwrap();
    assert!(prompt_text.contains("Check Australian Hisense PX3 prices"));
    assert!(prompt_text.contains("VOGT_NOTIFY:"));

    let detail: Value = client
        .get(format!("{base}/api/agent-tasks/{task_id}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(detail["run_count"], 1);
    assert_eq!(detail["runs"][0]["session_id"], session_id);

    let detail_after_exit: Value = loop {
        tokio::time::sleep(Duration::from_millis(40)).await;
        let detail: Value = client
            .get(format!("{base}/api/agent-tasks/{task_id}"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        if detail["runs"][0]["status"] == "completed" {
            break detail;
        }
    };
    assert_eq!(detail_after_exit["runs"][0]["status"], "completed");
    assert_eq!(detail_after_exit["runs"][0]["exit_code"], 0);
    assert!(detail_after_exit["runs"][0]["completed_at"]
        .as_str()
        .is_some());
    assert_eq!(
        detail_after_exit["runs"][0]["summary"],
        "Exited successfully"
    );

    let sessions: Vec<Value> = client
        .get(format!("{base}/api/sessions"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(sessions.iter().any(|s| s["id"] == session_id));
}

/// FR-E7, both halves in one run.
///
/// The binding reaches the run — the command prints the two environment
/// variables back, so a run that was told nothing would print a blank line —
/// and the notify phrase becomes a *recorded* finding on the run rather than
/// only a push. The whole point of the requirement is that a finding
/// survives the notification, so the assertion is that it is still there
/// afterwards, on the task, where a sweep can collect it.
#[tokio::test]
async fn a_bound_task_carries_its_subject_and_records_what_it_reported() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = test_config();
    cfg.default_cwd = tmp.path().to_path_buf();
    cfg.workspace_root = tmp.path().canonicalize().unwrap();
    cfg.state_dir = tmp.path().join("state");

    let (base, _h) = boot_with_config(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    let created: Value = client
        .post(format!("{base}/api/agent-tasks"))
        .json(&json!({
            "name": "Nightly dependency audit",
            "prompt": "Look for unresolved internal references.",
            "schedule": { "kind": "manual" },
            "vogt_project": "vogt",
            "vogt_work_item": "WI-7",
            // The sleep is not decoration: the watcher subscribes just after
            // the session is created, and a `printf` that finished first
            // would be a race rather than a test.
            "command": ["/bin/sh", "-lc",
                "sleep 0.3; printf 'MYDEVENV2_NOTIFY: bound to %s in %s\\n' \"$VOGT_WORK_ITEM\" \"$VOGT_PROJECT\""],
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let task_id = created["id"].as_str().unwrap().to_string();
    assert_eq!(created["vogt_project"], "vogt");
    assert_eq!(created["vogt_work_item"], "WI-7");

    let run: Value = client
        .post(format!("{base}/api/agent-tasks/{task_id}/run"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let prompt_text = std::fs::read_to_string(run["prompt_file"].as_str().unwrap()).unwrap();
    assert!(
        prompt_text.contains("Vogt subject: WI-7 (project vogt)"),
        "the run's own prompt must name what it is about: {prompt_text}"
    );

    let detail = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            tokio::time::sleep(Duration::from_millis(50)).await;
            let detail: Value = client
                .get(format!("{base}/api/agent-tasks/{task_id}"))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            if detail["runs"][0]["findings"]
                .as_array()
                .is_some_and(|f| !f.is_empty())
            {
                break detail;
            }
        }
    })
    .await
    .expect("the notify phrase should have produced a finding");

    let finding = &detail["runs"][0]["findings"][0];
    assert_eq!(finding["text"], "bound to WI-7 in vogt");
    assert_eq!(finding["source"], "notify-phrase");
    assert!(finding["at"].as_str().is_some());
}

/// An unbound task is the engine's own business, and says nothing about Vogt.
#[tokio::test]
async fn an_unbound_task_names_no_vogt_subject() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = test_config();
    cfg.default_cwd = tmp.path().to_path_buf();
    cfg.workspace_root = tmp.path().canonicalize().unwrap();
    cfg.state_dir = tmp.path().join("state");

    let (base, _h) = boot_with_config(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    let created: Value = client
        .post(format!("{base}/api/agent-tasks"))
        .json(&json!({
            "name": "Unbound",
            "prompt": "Do something for its own sake.",
            "schedule": { "kind": "manual" },
            "command": ["/bin/sh", "-lc", "true"],
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(created.get("vogt_project").is_none());
    assert!(created.get("vogt_work_item").is_none());

    let run: Value = client
        .post(format!(
            "{base}/api/agent-tasks/{}/run",
            created["id"].as_str().unwrap()
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let prompt_text = std::fs::read_to_string(run["prompt_file"].as_str().unwrap()).unwrap();
    assert!(!prompt_text.contains("Vogt subject"));
}

/// #296: the fake-agent is registered as a session preset, so a client can
/// discover it the same way it discovers the real agent templates.
#[tokio::test]
async fn fake_agent_is_registered_as_a_session_preset() {
    let (base, _h) = boot().await;
    let cfg: Value = reqwest::get(format!("{base}/api/config"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let templates = cfg["session_templates"]
        .as_array()
        .expect("config advertises session templates");
    assert!(
        templates.iter().any(|t| t["name"] == "Fake Agent (test)"),
        "the synthetic agent preset should be listed: {templates:?}"
    );
}

/// #296 edit+commit, driven all the way through the engine's agent-task path.
///
/// The run's working tree is a real git repo; the fake-agent edits a file and
/// commits it with the checkpoint trailers, and the trailer carrying the run
/// id must equal the run the engine actually started. This is the seam the
/// future git story (#283-#285) needs: a checkpoint a run made, traceable back
/// to that run, produced without a real agent CLI.
#[tokio::test]
async fn fake_agent_edit_commit_scenario_leaves_a_trailered_commit() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    // A committable repo with an identity, so the commit does not depend on the
    // CI user's git config.
    for args in [
        vec!["init", "-q"],
        vec!["config", "user.email", "seed@vogt.invalid"],
        vec!["config", "user.name", "seed"],
    ] {
        let status = std::process::Command::new("git")
            .args(&args)
            .current_dir(&repo)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?} failed");
    }

    let mut cfg = test_config();
    cfg.default_cwd = tmp.path().to_path_buf();
    cfg.workspace_root = tmp.path().canonicalize().unwrap();
    cfg.state_dir = tmp.path().join("state");
    let (base, _h) = boot_with_config(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    let created: Value = client
        .post(format!("{base}/api/agent-tasks"))
        .json(&json!({
            "name": "fake edit+commit",
            "prompt": "Make a checkpoint.",
            "schedule": { "kind": "manual" },
            "command": fake_agent_command("edit+commit"),
            "cwd": repo.to_string_lossy(),
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let task_id = created["id"].as_str().unwrap().to_string();

    let run: Value = client
        .post(format!("{base}/api/agent-tasks/{task_id}/run"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let run_id = run["id"].as_str().unwrap().to_string();

    let detail = wait_for_run_finish(&client, &base, &task_id).await;
    assert_eq!(detail["runs"][0]["status"], "completed");

    // The engine set MYDEVENV2_AGENT_TASK_RUN_ID for the run; the fake-agent
    // wrote it into a `Vogt-Run` trailer on the commit it made.
    let trailer = std::process::Command::new("git")
        .args(["log", "-1", "--pretty=%(trailers:key=Vogt-Run,valueonly)"])
        .current_dir(&repo)
        .output()
        .unwrap();
    let trailer = String::from_utf8_lossy(&trailer.stdout).trim().to_string();
    assert_eq!(
        trailer, run_id,
        "the checkpoint commit should carry the engine's run id as a trailer"
    );
}

/// #296 findings, driven through the engine's phrase watcher.
///
/// The fake-agent prints a `VOGT_NOTIFY:` line; the engine records the text
/// after it as a durable finding on the run (FR-E7). No push service, no real
/// agent — just the parse-and-record path #291 will build on.
#[tokio::test]
async fn fake_agent_findings_scenario_is_recorded_on_the_run() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = test_config();
    cfg.default_cwd = tmp.path().to_path_buf();
    cfg.workspace_root = tmp.path().canonicalize().unwrap();
    cfg.state_dir = tmp.path().join("state");
    let (base, _h) = boot_with_config(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    let created: Value = client
        .post(format!("{base}/api/agent-tasks"))
        .json(&json!({
            "name": "fake findings",
            "prompt": "Report something.",
            "schedule": { "kind": "manual" },
            "command": fake_agent_command("findings"),
            "env": [["FAKE_AGENT_NOTIFY_TEXT", "the price dropped"]],
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let task_id = created["id"].as_str().unwrap().to_string();

    client
        .post(format!("{base}/api/agent-tasks/{task_id}/run"))
        .send()
        .await
        .unwrap();

    let detail = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            tokio::time::sleep(Duration::from_millis(50)).await;
            let detail: Value = client
                .get(format!("{base}/api/agent-tasks/{task_id}"))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            if detail["runs"][0]["findings"]
                .as_array()
                .is_some_and(|f| !f.is_empty())
            {
                break detail;
            }
        }
    })
    .await
    .expect("the VOGT_NOTIFY line should have produced a finding");

    let finding = &detail["runs"][0]["findings"][0];
    assert_eq!(finding["text"], "the price dropped");
    assert_eq!(finding["source"], "notify-phrase");
}

/// #296 outcome: a chosen non-zero exit is surfaced as an errored run.
#[tokio::test]
async fn fake_agent_outcome_scenario_surfaces_the_exit_code() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = test_config();
    cfg.default_cwd = tmp.path().to_path_buf();
    cfg.workspace_root = tmp.path().canonicalize().unwrap();
    cfg.state_dir = tmp.path().join("state");
    let (base, _h) = boot_with_config(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    let created: Value = client
        .post(format!("{base}/api/agent-tasks"))
        .json(&json!({
            "name": "fake outcome",
            "prompt": "Exit non-zero.",
            "schedule": { "kind": "manual" },
            "command": fake_agent_command("outcome"),
            "env": [["FAKE_AGENT_EXIT_CODE", "7"]],
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let task_id = created["id"].as_str().unwrap().to_string();

    client
        .post(format!("{base}/api/agent-tasks/{task_id}/run"))
        .send()
        .await
        .unwrap();

    let detail = wait_for_run_finish(&client, &base, &task_id).await;
    assert_eq!(detail["runs"][0]["status"], "errored");
    assert_eq!(detail["runs"][0]["exit_code"], 7);
    assert_eq!(detail["runs"][0]["summary"], "Exited with status 7");
}

/// #290: a `work.transitioned` core event starts a bound, audited run.
///
/// The engine subscribes to its own bus — the one the core-event follower
/// feeds — matches the enabled `work-transition` trigger, and starts a NORMAL
/// run. The run is bound to the item that transitioned (its `VOGT_WORK_ITEM`
/// reaches the child and its prompt names it), and its audit names the trigger
/// and the event that fired it, so a `why` could say "ran because WI-7 entered
/// ready at seq 4102".
#[tokio::test]
async fn a_work_transition_event_starts_a_bound_and_audited_run() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = test_config();
    cfg.default_cwd = tmp.path().to_path_buf();
    cfg.workspace_root = tmp.path().canonicalize().unwrap();
    cfg.state_dir = tmp.path().join("state");
    let (base, state, _h) = boot_with_state(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    let created: Value = client
        .post(format!("{base}/api/agent-tasks"))
        .json(&json!({
            "name": "ready-watcher",
            "prompt": "A work item reached ready.",
            "schedule": { "kind": "manual" },
            "command": ["/bin/sh", "-lc",
                "sleep 0.2; printf 'bound=%s\\n' \"$VOGT_WORK_ITEM\""],
            "triggers": [
                { "enabled": true, "kind": "work-transition", "to_state": "ready" }
            ],
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let task_id = created["id"].as_str().unwrap().to_string();
    // The trigger round-trips on the task.
    assert_eq!(created["triggers"][0]["kind"], "work-transition");
    assert_eq!(created["triggers"][0]["to_state"], "ready");
    assert_eq!(created["concurrency"], 1);

    // A transition to a *different* state does not fire the trigger.
    publish_core_event(
        &state,
        "work.transitioned",
        "wi_9",
        4100,
        json!({"ref": "WI-9", "from": "open", "to": "in_progress"}),
    );
    // The matching transition does.
    publish_core_event(
        &state,
        "work.transitioned",
        "wi_7",
        4102,
        json!({"ref": "WI-7", "from": "review", "to": "ready"}),
    );

    let detail = wait_for_run_count(&client, &base, &task_id, 1).await;
    // Only the matching event started a run.
    assert_eq!(detail["runs"].as_array().unwrap().len(), 1);
    let run = &detail["runs"][0];
    assert_eq!(run["trigger"], "event");
    // The audit names the trigger and the exact event that fired it.
    assert_eq!(run["trigger_detail"]["trigger_kind"], "work-transition");
    assert_eq!(run["trigger_detail"]["event_kind"], "work.transitioned");
    assert_eq!(run["trigger_detail"]["event_id"], "wi_7");
    assert_eq!(run["trigger_detail"]["event_seq"], 4102);
    assert_eq!(run["trigger_detail"]["description"], "WI-7 entered ready");

    // The run is bound to the item that transitioned: its prompt names it.
    let prompt = std::fs::read_to_string(run["prompt_file"].as_str().unwrap()).unwrap();
    assert!(
        prompt.contains("Vogt subject: WI-7"),
        "the triggered run must be bound to the item that fired it: {prompt}"
    );
}

/// #290: drift, observation, and forge-PR-check events each start the right
/// run, driven by synthetic core events.
#[tokio::test]
async fn drift_observation_and_forge_events_each_start_a_run() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = test_config();
    cfg.default_cwd = tmp.path().to_path_buf();
    cfg.workspace_root = tmp.path().canonicalize().unwrap();
    cfg.state_dir = tmp.path().join("state");
    let (base, state, _h) = boot_with_state(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    // Each case: the trigger to arm, the event that should fire it, and the
    // audit description the run should carry.
    struct Case {
        name: &'static str,
        trigger: Value,
        event_kind: &'static str,
        entity_id: &'static str,
        summary: Value,
        expect_desc: &'static str,
    }
    let cases = [
        Case {
            name: "drift-watcher",
            trigger: json!({"enabled": true, "kind": "drift-proposed", "project": "vogt"}),
            event_kind: "drift.raised",
            entity_id: "d_1",
            summary: json!({"kind": "coupling", "summary": "x leaks into y", "project": "vogt"}),
            expect_desc: "drift raised: x leaks into y",
        },
        Case {
            name: "issue-watcher",
            trigger: json!({"enabled": true, "kind": "observation-new", "observation_kind": "forge.issue"}),
            event_kind: "observation.new",
            entity_id: "obs_1",
            summary: json!({"kind": "forge.issue", "project": "vogt", "subject": "#42"}),
            expect_desc: "new forge.issue: #42",
        },
        Case {
            name: "checks-watcher",
            trigger: json!({"enabled": true, "kind": "forge-pr-checks", "status": "red"}),
            event_kind: "forge.pr.checks",
            entity_id: "pr_12",
            summary: json!({"status": "red", "work_item": "WI-7", "pr": "#12"}),
            expect_desc: "PR #12 checks red",
        },
    ];

    let mut seq = 5000;
    for case in cases {
        let created: Value = client
            .post(format!("{base}/api/agent-tasks"))
            .json(&json!({
                "name": case.name,
                "prompt": "React.",
                "schedule": { "kind": "manual" },
                "command": ["/bin/sh", "-lc", "true"],
                "triggers": [case.trigger],
            }))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let task_id = created["id"].as_str().unwrap().to_string();

        seq += 1;
        publish_core_event(
            &state,
            case.event_kind,
            case.entity_id,
            seq,
            case.summary.clone(),
        );

        let detail = wait_for_run_count(&client, &base, &task_id, 1).await;
        let run = &detail["runs"][0];
        assert_eq!(
            run["trigger"], "event",
            "{} should be event-triggered",
            case.name
        );
        assert_eq!(run["trigger_detail"]["event_kind"], case.event_kind);
        assert_eq!(run["trigger_detail"]["description"], case.expect_desc);
    }
}

/// #290: the per-task concurrency cap holds, and a fire it cannot honour is
/// dropped rather than retried (no storm).
#[tokio::test]
async fn the_concurrency_cap_holds_and_a_blocked_fire_does_not_storm() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = test_config();
    cfg.default_cwd = tmp.path().to_path_buf();
    cfg.workspace_root = tmp.path().canonicalize().unwrap();
    cfg.state_dir = tmp.path().join("state");
    let (base, state, _h) = boot_with_state(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    // A task capped at one run, whose run sleeps long enough to still be in
    // flight when the second event arrives.
    let created: Value = client
        .post(format!("{base}/api/agent-tasks"))
        .json(&json!({
            "name": "capped",
            "prompt": "Only one at a time.",
            "schedule": { "kind": "manual" },
            "concurrency": 1,
            "command": ["/bin/sh", "-lc", "sleep 3"],
            "triggers": [
                { "enabled": true, "kind": "work-transition", "to_state": "ready" }
            ],
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let task_id = created["id"].as_str().unwrap().to_string();

    // First fire starts a run.
    publish_core_event(
        &state,
        "work.transitioned",
        "wi_7",
        6001,
        json!({"ref": "WI-7", "to": "ready"}),
    );
    let detail = wait_for_run_count(&client, &base, &task_id, 1).await;
    assert_eq!(detail["runs"][0]["status"], "running");

    // Second fire while the first run is still in flight: the cap is full, so
    // it is dropped, not queued and not retried.
    publish_core_event(
        &state,
        "work.transitioned",
        "wi_8",
        6002,
        json!({"ref": "WI-8", "to": "ready"}),
    );

    // Give the watcher ample time to (not) start a second run.
    tokio::time::sleep(Duration::from_millis(600)).await;
    let detail: Value = client
        .get(format!("{base}/api/agent-tasks/{task_id}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        detail["runs"].as_array().unwrap().len(),
        1,
        "the capped task must not start an overlapping run, and must not spin: {:?}",
        detail["runs"]
    );
    assert_eq!(detail["run_count"], 1);
}

/// #290: an `api` fire is refused unless the task has an `api` trigger armed,
/// and records `api` when it is.
#[tokio::test]
async fn an_api_fire_needs_an_api_trigger_and_records_api() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = test_config();
    cfg.default_cwd = tmp.path().to_path_buf();
    cfg.workspace_root = tmp.path().canonicalize().unwrap();
    cfg.state_dir = tmp.path().join("state");
    let (base, _h) = boot_with_config(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    // No api trigger yet: a programmatic fire is refused.
    let created: Value = client
        .post(format!("{base}/api/agent-tasks"))
        .json(&json!({
            "name": "api-task",
            "prompt": "Fire me from a script.",
            "schedule": { "kind": "manual" },
            "command": ["/bin/sh", "-lc", "true"],
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let task_id = created["id"].as_str().unwrap().to_string();

    let refused = client
        .post(format!("{base}/api/agent-tasks/{task_id}/run?trigger=api"))
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), StatusCode::CONFLICT);

    // Arm an api trigger, then the fire records `api`.
    client
        .patch(format!("{base}/api/agent-tasks/{task_id}"))
        .json(&json!({ "triggers": [{ "enabled": true, "kind": "api" }] }))
        .send()
        .await
        .unwrap();

    let run: Value = client
        .post(format!("{base}/api/agent-tasks/{task_id}/run?trigger=api"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(run["trigger"], "api");
    assert_eq!(run["trigger_detail"]["trigger_kind"], "api");

    // Let the api run's session exit so the (concurrency-capped) task is free
    // to run again — otherwise the manual fire below would race it and 409.
    wait_for_run_finish(&client, &base, &task_id).await;

    // A plain human Run Now on the same task still records `manual`.
    let manual: Value = client
        .post(format!("{base}/api/agent-tasks/{task_id}/run"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(manual["trigger"], "manual");
}

#[tokio::test]
async fn gui_launch_lists_and_kills() {
    let (base, _h) = boot().await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let launched: Value = client
        .post(format!("{base}/api/gui/launch"))
        .json(&json!({ "command": ["sleep", "10"], "via_sway": false }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let pid = launched["pid"].as_u64().expect("pid in launch response");

    let procs: Vec<Value> = client
        .get(format!("{base}/api/gui/processes"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        procs.iter().any(|p| p["pid"].as_u64() == Some(pid)),
        "expected pid {pid} in {procs:?}"
    );

    let k = client
        .post(format!("{base}/api/gui/kill?pid={pid}"))
        .send()
        .await
        .unwrap();
    assert_eq!(k.status(), StatusCode::OK);
}

#[tokio::test]
async fn list_sessions_rejects_missing_auth() {
    let (base, _h) = boot().await;
    let res = reqwest::get(format!("{base}/api/sessions")).await.unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn list_sessions_rejects_wrong_token() {
    let (base, _h) = boot().await;
    let client = reqwest::Client::new();
    let res = client
        .get(format!("{base}/api/sessions"))
        .header("Authorization", "Bearer wrong-token")
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn create_list_and_kill_session() {
    let (base, _h) = boot().await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    let create: Value = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({
            "name": "test-shell",
            "command": ["/bin/cat"],
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = create["id"].as_str().unwrap().to_string();
    assert_eq!(create["name"], "test-shell");

    let list: Vec<Value> = client
        .get(format!("{base}/api/sessions"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(list.iter().any(|s| s["id"] == id));

    let kill = client
        .post(format!("{base}/api/sessions/{id}/kill"))
        .send()
        .await
        .unwrap();
    assert_eq!(kill.status(), StatusCode::OK);

    let del = client
        .delete(format!("{base}/api/sessions/{id}"))
        .send()
        .await
        .unwrap();
    assert_eq!(del.status(), StatusCode::OK);
}

#[tokio::test]
async fn rename_session() {
    let (base, _h) = boot().await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    let id: String = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({ "name": "before", "command": ["/bin/cat"] }))
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    let r = client
        .patch(format!("{base}/api/sessions/{id}"))
        .json(&json!({ "name": "after" }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);

    let detail: Value = client
        .get(format!("{base}/api/sessions/{id}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(detail["summary"]["name"], "after");

    let _ = client
        .delete(format!("{base}/api/sessions/{id}"))
        .send()
        .await;
}

#[tokio::test]
async fn create_session_trims_name() {
    let (base, _h) = boot().await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    let create: Value = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({ "name": "  trimmed shell  ", "command": ["/bin/cat"] }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(create["name"], "trimmed shell");

    let id = create["id"].as_str().unwrap();
    let _ = client
        .delete(format!("{base}/api/sessions/{id}"))
        .send()
        .await;
}

#[tokio::test]
async fn rename_session_trims_name() {
    let (base, _h) = boot().await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    let id: String = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({ "name": "before", "command": ["/bin/cat"] }))
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    let r = client
        .patch(format!("{base}/api/sessions/{id}"))
        .json(&json!({ "name": "  after trim  " }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);

    let detail: Value = client
        .get(format!("{base}/api/sessions/{id}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(detail["summary"]["name"], "after trim");

    let _ = client
        .delete(format!("{base}/api/sessions/{id}"))
        .send()
        .await;
}

#[tokio::test]
async fn session_name_limit_is_enforced() {
    let (base, _h) = boot().await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let long = "a".repeat(257);

    let create = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({ "name": long, "command": ["/bin/cat"] }))
        .send()
        .await
        .unwrap();
    assert_eq!(create.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn session_cwd_must_stay_under_workspace_root() {
    let tmp = tempfile::tempdir().unwrap();
    let workspace = tmp.path().join("workspace");
    let nested = workspace.join("nested");
    std::fs::create_dir_all(&nested).unwrap();
    let outside = tmp.path().join("outside");
    std::fs::create_dir_all(&outside).unwrap();

    let mut cfg = test_config();
    cfg.default_cwd = workspace.clone();
    cfg.workspace_root = workspace.canonicalize().unwrap();

    let (base, _h) = boot_with_config(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    let create: Value = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({
            "name": "cwd-test",
            "command": ["/bin/cat"],
            "cwd": nested.canonicalize().unwrap().to_string_lossy().into_owned()
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        create["cwd"],
        nested.canonicalize().unwrap().to_string_lossy().as_ref()
    );
    let id = create["id"].as_str().unwrap();
    let _ = client
        .delete(format!("{base}/api/sessions/{id}"))
        .send()
        .await;

    let rejected = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({
            "name": "bad-cwd",
            "command": ["/bin/cat"],
            "cwd": outside.canonicalize().unwrap().to_string_lossy().into_owned()
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(rejected.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn session_activity_becomes_idle_after_quiet_window() {
    let (base, _h) = boot().await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    let create: Value = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({
            "name": "idle-watch",
            "command": ["/bin/sh", "-lc", "printf ready; sleep 1"],
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = create["id"].as_str().unwrap().to_string();

    let detail: Value = loop {
        let detail: Value = client
            .get(format!("{base}/api/sessions/{id}"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        if detail["summary"]["activity"] == "idle" {
            break detail;
        }
        tokio::time::sleep(Duration::from_millis(40)).await;
    };
    assert_eq!(detail["summary"]["activity"], "idle");

    let _ = client
        .delete(format!("{base}/api/sessions/{id}"))
        .send()
        .await;
}

#[tokio::test]
async fn get_session_returns_typed_detail_shape() {
    let (base, _h) = boot().await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    let created: Value = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({ "name": "detail-shape" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = created["id"].as_str().unwrap();

    let detail: SessionDetail = client
        .get(format!("{base}/api/sessions/{id}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(detail.summary.id.to_string(), id);
    assert_eq!(detail.summary.name, "detail-shape");
    assert!(detail.summary.created_at.contains('T'));

    let _ = client
        .delete(format!("{base}/api/sessions/{id}"))
        .send()
        .await;
}

#[tokio::test]
async fn create_session_accepts_scrollback_override() {
    let (base, _h) = boot().await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    let created: Value = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({
            "name": "scrollback-override",
            "command": ["/bin/sh", "-lc", "printf 'abcdefghijk'"],
            "scrollback_bytes": 8,
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = created["id"].as_str().unwrap().to_string();

    // The child can exit before the PTY reader has drained what it wrote:
    // process exit and output accounting complete independently, exactly as
    // archival and indexing do in the history test below. Waiting only for
    // `exit_code` therefore observed a scrollback of 0 about one run in five
    // — a flake that only became visible once CI started running this suite
    // on every push. Poll until the accounting settles, on a deadline, so a
    // count that is genuinely wrong still fails rather than hanging.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let detail: SessionDetail = loop {
        let detail: SessionDetail = client
            .get(format!("{base}/api/sessions/{id}"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        if detail.summary.exit_code.is_some() && detail.summary.scrollback_bytes == 11 {
            break detail;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "session never accounted for the 11 bytes it wrote; \
             scrollback_bytes={}, exit_code={:?}",
            detail.summary.scrollback_bytes,
            detail.summary.exit_code
        );
        tokio::time::sleep(Duration::from_millis(40)).await;
    };

    let snapshot = base64::engine::general_purpose::STANDARD
        .decode(detail.scrollback_base64.as_bytes())
        .unwrap();
    assert_eq!(detail.summary.scrollback_bytes, 11);
    assert_eq!(snapshot, b"defghijk");
}

#[tokio::test]
async fn session_child_receives_its_own_session_id() {
    // The session id is allocated before the spawn so the child can be told
    // which session it is: `MYDEVENV2_SESSION` is only the display name, which
    // is not unique and cannot identify a session.
    let (base, _h) = boot().await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    let created: Value = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({
            "name": "session-id-env",
            "command": ["/bin/sh", "-lc", "printf %s \"$MYDEVENV2_SESSION_ID\""],
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = created["id"].as_str().unwrap().to_string();

    let detail: SessionDetail = loop {
        let detail: SessionDetail = client
            .get(format!("{base}/api/sessions/{id}"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        if detail.summary.exit_code.is_some() {
            break detail;
        }
        tokio::time::sleep(Duration::from_millis(40)).await;
    };

    let snapshot = base64::engine::general_purpose::STANDARD
        .decode(detail.scrollback_base64.as_bytes())
        .unwrap();
    let printed = String::from_utf8_lossy(&snapshot);
    assert!(
        printed.contains(&id),
        "child should see its own session id; got {printed:?}"
    );
}

#[tokio::test]
async fn exited_sessions_are_archived_searchable_and_deletable() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = test_config();
    cfg.default_cwd = tmp.path().to_path_buf();
    cfg.workspace_root = tmp.path().canonicalize().unwrap();

    let (base, _h) = boot_with_config(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    let id: String = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({
            "name": "archive-me",
            "command": [
                "/bin/sh",
                "-lc",
                "printf 'history-needle path/with punctuation\\n'; exit 7",
            ],
        }))
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let archived = loop {
        let sessions: Vec<Value> = client
            .get(format!("{base}/api/history/sessions?limit=20"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        if let Some(session) = sessions.iter().find(|s| s["id"] == id) {
            break session.clone();
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "session was not archived; got {sessions:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    };

    assert_eq!(archived["name"], "archive-me");
    assert_eq!(archived["exit_code"], 7);
    assert!(
        archived["scrollback_bytes"].as_i64().unwrap_or_default() > 0,
        "archive should record output bytes: {archived:?}"
    );

    // The session record appearing in /history/sessions does not imply its
    // output is searchable: archival and indexing complete independently, so
    // the poll above settles only the first of them. A bare read here passes
    // on an idle machine and loses under load — pipeline #194 failed exactly
    // this way while the commit under test touched only .dockerignore. Same
    // deadline discipline as the loop above.
    let search_deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let hits: Vec<Value> = client
            .get(format!(
                "{base}/api/history/search?q=history-needle%20path%2Fwith"
            ))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        if hits.iter().any(|hit| hit["session_id"] == id) {
            break;
        }
        assert!(
            tokio::time::Instant::now() < search_deadline,
            "history search should find archived output; got {hits:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let del = client
        .delete(format!("{base}/api/history/{id}"))
        .send()
        .await
        .unwrap();
    assert_eq!(del.status(), StatusCode::OK);

    let after_delete = client
        .get(format!("{base}/api/history/{id}"))
        .send()
        .await
        .unwrap();
    assert_eq!(after_delete.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn archived_history_log_preview_and_download_work() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = test_config();
    cfg.default_cwd = tmp.path().to_path_buf();
    cfg.workspace_root = tmp.path().canonicalize().unwrap();

    let (base, _h) = boot_with_config(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    let id: String = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({
            "name": "replay-me",
            "command": [
                "/bin/sh",
                "-lc",
                "printf 'first line\\nsecond line\\nthird line\\n'; exit 0",
            ],
        }))
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let session = client
            .get(format!("{base}/api/history/{id}"))
            .send()
            .await
            .unwrap();
        if session.status() == StatusCode::OK {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "session was not archived in time"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let preview: Value = client
        .get(format!("{base}/api/history/{id}/log?tail_bytes=12"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(preview["session_id"], id);
    assert_eq!(preview["truncated"], true);
    assert_eq!(preview["bytes"], 12);
    assert!(
        preview["text"]
            .as_str()
            .unwrap_or("")
            .contains("third line"),
        "preview should contain tail output: {preview:?}"
    );

    let download = client
        .get(format!("{base}/api/history/{id}/download"))
        .send()
        .await
        .unwrap();
    assert_eq!(download.status(), StatusCode::OK);
    let content_disposition = download
        .headers()
        .get(reqwest::header::CONTENT_DISPOSITION)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    assert!(
        content_disposition.contains("attachment"),
        "download should set attachment disposition: {content_disposition}"
    );
    let body = download.text().await.unwrap();
    assert!(body.contains("first line"));
    assert!(body.contains("third line"));
}

#[tokio::test]
async fn archived_history_cleanup_removes_old_sessions_and_logs() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = test_config();
    cfg.default_cwd = tmp.path().to_path_buf();
    cfg.workspace_root = tmp.path().canonicalize().unwrap();
    cfg.state_dir = tmp.path().join("state");

    let (base, _h) = boot_with_config(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    let id: String = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({
            "name": "cleanup-me",
            "command": ["/bin/sh", "-lc", "printf 'cleanup-history\\n'; exit 0"],
        }))
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let session = client
            .get(format!("{base}/api/history/{id}"))
            .send()
            .await
            .unwrap();
        if session.status() == StatusCode::OK {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "session was not archived in time"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let cleanup: Value = client
        .post(format!("{base}/api/history/cleanup"))
        .json(&json!({ "retention_days": 0 }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(cleanup["ok"], true);
    assert_eq!(cleanup["removed_sessions"], 1);

    let after = client
        .get(format!("{base}/api/history/{id}"))
        .send()
        .await
        .unwrap();
    assert_eq!(after.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn task_prompt_artifact_cleanup_prunes_old_runs_and_orphans() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = test_config();
    cfg.default_cwd = tmp.path().to_path_buf();
    cfg.workspace_root = tmp.path().canonicalize().unwrap();
    cfg.state_dir = tmp.path().join("state");

    let (base, _h) = boot_with_config(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    let created: Value = client
        .post(format!("{base}/api/agent-tasks"))
        .json(&json!({
            "name": "artifact cleanup",
            "prompt": "Keep the latest prompt only.",
            "schedule": { "kind": "manual" },
            "command": ["/bin/sh", "-lc", "printf 'done\\n'"],
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let task_id = created["id"].as_str().unwrap().to_string();

    let mut prompt_files: Vec<String> = Vec::new();
    for _ in 0..2 {
        let run_deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            let detail: Value = client
                .get(format!("{base}/api/agent-tasks/{task_id}"))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            let latest_running = detail["runs"]
                .as_array()
                .and_then(|runs| runs.last())
                .map(|run| run["status"] == "running")
                .unwrap_or(false);
            if !latest_running {
                break;
            }
            assert!(
                tokio::time::Instant::now() < run_deadline,
                "prior task run did not finish in time"
            );
            tokio::time::sleep(Duration::from_millis(40)).await;
        }
        let run: Value = client
            .post(format!("{base}/api/agent-tasks/{task_id}/run"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        prompt_files.push(run["prompt_file"].as_str().unwrap().to_string());
    }

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let detail: Value = client
            .get(format!("{base}/api/agent-tasks/{task_id}"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let all_finished = detail["runs"]
            .as_array()
            .map(|runs| runs.iter().all(|run| run["status"] != "running"))
            .unwrap_or(false);
        if all_finished {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "task runs did not finish in time"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let cleanup_runs: Value = client
        .post(format!("{base}/api/agent-tasks/artifacts/cleanup"))
        .json(&json!({ "keep_latest_runs_per_task": 1 }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(cleanup_runs["removed_prompt_file_count"], 1);
    assert!(!std::path::Path::new(&prompt_files[0]).exists());
    assert!(std::path::Path::new(&prompt_files[1]).exists());

    let deleted = client
        .delete(format!("{base}/api/agent-tasks/{task_id}"))
        .send()
        .await
        .unwrap();
    assert_eq!(deleted.status(), StatusCode::OK);

    let orphan_dir = tmp
        .path()
        .join("state")
        .join("agent-task-prompts")
        .join("orphan-task");
    std::fs::create_dir_all(&orphan_dir).unwrap();
    std::fs::write(orphan_dir.join("stale.md"), "stale").unwrap();

    let cleanup_orphans: Value = client
        .post(format!("{base}/api/agent-tasks/artifacts/cleanup"))
        .json(&json!({ "keep_latest_runs_per_task": 1 }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(cleanup_orphans["removed_task_dir_count"], 1);
    let task_dir = tmp
        .path()
        .join("state")
        .join("agent-task-prompts")
        .join(task_id);
    assert!(!task_dir.exists());
    assert!(!orphan_dir.exists());
}

/// A session's brief lands in a file, and the child is told where it is —
/// never handed the text. Vogt writes the work item's brief this way because
/// it is a separate process and the file belongs on the engine's state dir.
#[tokio::test]
async fn session_prompt_is_written_to_a_file_the_child_is_pointed_at() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = test_config();
    cfg.default_cwd = tmp.path().to_path_buf();
    cfg.workspace_root = tmp.path().canonicalize().unwrap();
    cfg.state_dir = tmp.path().join("state");

    let (base, _h) = boot_with_config(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    let brief = "Fix the flaky forge test.\n\nWhy: it blocks the release.";
    let created: Value = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({
            "name": "work item 42",
            "prompt": brief,
            "command": ["/bin/sh", "-lc",
                "printf 'file=[%s]\\n' \"$MYDEVENV2_AGENT_TASK_PROMPT_FILE\"; \
                 cat \"$MYDEVENV2_AGENT_TASK_PROMPT_FILE\""],
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = created["id"].as_str().unwrap().to_string();

    let prompt_path = tmp
        .path()
        .join("state")
        .join("agent-task-prompts")
        .join("sessions")
        .join(format!("{id}.md"));
    assert_eq!(std::fs::read_to_string(&prompt_path).unwrap(), brief);

    let printed = session_output_after_exit(&client, &base, &id).await;
    assert!(
        printed.contains(&format!("file=[{}]", prompt_path.display())),
        "child should be told the prompt file path; got {printed:?}"
    );
    assert!(
        printed.contains("Fix the flaky forge test."),
        "child should be able to read the brief; got {printed:?}"
    );
}

#[tokio::test]
async fn a_session_without_a_prompt_gets_no_file_and_no_variable() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = test_config();
    cfg.default_cwd = tmp.path().to_path_buf();
    cfg.workspace_root = tmp.path().canonicalize().unwrap();
    cfg.state_dir = tmp.path().join("state");

    let (base, _h) = boot_with_config(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    let created: Value = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({
            "name": "plain terminal",
            // Whitespace is not a brief: this must behave exactly like a
            // request that omits the field.
            "prompt": "   ",
            "command": ["/bin/sh", "-lc",
                "printf 'file=[%s]\\n' \"$MYDEVENV2_AGENT_TASK_PROMPT_FILE\""],
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = created["id"].as_str().unwrap().to_string();

    let printed = session_output_after_exit(&client, &base, &id).await;
    assert!(
        printed.contains("file=[]"),
        "no brief means no variable; got {printed:?}"
    );
    let sessions_dir = tmp
        .path()
        .join("state")
        .join("agent-task-prompts")
        .join("sessions");
    assert!(
        !sessions_dir.join(format!("{id}.md")).exists(),
        "no brief means no prompt file"
    );
}

#[tokio::test]
async fn deleting_a_session_forgets_its_prompt_file() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = test_config();
    cfg.default_cwd = tmp.path().to_path_buf();
    cfg.workspace_root = tmp.path().canonicalize().unwrap();
    cfg.state_dir = tmp.path().join("state");

    let (base, _h) = boot_with_config(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    let created: Value = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({
            "name": "work item 43",
            "prompt": "Land the migration.",
            "command": ["/bin/cat"],
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = created["id"].as_str().unwrap().to_string();

    let prompt_path = tmp
        .path()
        .join("state")
        .join("agent-task-prompts")
        .join("sessions")
        .join(format!("{id}.md"));
    assert!(prompt_path.exists());

    // Killing keeps the session inspectable, and its brief with it.
    let killed = client
        .post(format!("{base}/api/sessions/{id}/kill"))
        .send()
        .await
        .unwrap();
    assert_eq!(killed.status(), StatusCode::OK);
    assert!(
        prompt_path.exists(),
        "a killed session is still inspectable; its brief stays"
    );

    let deleted = client
        .delete(format!("{base}/api/sessions/{id}"))
        .send()
        .await
        .unwrap();
    assert_eq!(deleted.status(), StatusCode::OK);
    assert!(
        !prompt_path.exists(),
        "forgetting the session forgets its brief"
    );
}

/// Prompt files whose session the registry no longer knows — the ones a crash
/// or a restart leaves behind — are collected by the same artifact cleanup
/// endpoint that prunes task run prompts.
#[tokio::test]
async fn artifact_cleanup_collects_prompts_of_sessions_the_registry_forgot() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = test_config();
    cfg.default_cwd = tmp.path().to_path_buf();
    cfg.workspace_root = tmp.path().canonicalize().unwrap();
    cfg.state_dir = tmp.path().join("state");

    let (base, _h) = boot_with_config(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    let created: Value = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({
            "name": "work item 44",
            "prompt": "Keep me: my session still exists.",
            "command": ["/bin/true"],
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = created["id"].as_str().unwrap().to_string();

    let sessions_dir = tmp
        .path()
        .join("state")
        .join("agent-task-prompts")
        .join("sessions");
    let live_prompt = sessions_dir.join(format!("{id}.md"));
    // Stands in for a prompt file written before a restart: the session id is
    // well formed, but the registry has never heard of it.
    let stale_prompt = sessions_dir.join(format!("{}.md", uuid::Uuid::new_v4()));
    std::fs::write(&stale_prompt, "brief of a session that no longer exists").unwrap();

    let status: Value = client
        .get(format!("{base}/api/status"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(status["agent_tasks"]["session_prompt_file_count"], 2);

    let cleanup: Value = client
        .post(format!("{base}/api/agent-tasks/artifacts/cleanup"))
        .json(&json!({ "keep_latest_runs_per_task": 10 }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(cleanup["removed_session_prompt_file_count"], 1);
    // The sessions directory is not a task directory and must never be swept
    // as an orphan one.
    assert_eq!(cleanup["removed_task_dir_count"], 0);
    assert!(!stale_prompt.exists());
    assert!(
        live_prompt.exists(),
        "a brief whose session the registry still holds must survive cleanup"
    );
}

/// Run a session to completion and return everything it printed.
async fn session_output_after_exit(client: &reqwest::Client, base: &str, id: &str) -> String {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let detail: SessionDetail = loop {
        let detail: SessionDetail = client
            .get(format!("{base}/api/sessions/{id}"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        if detail.summary.exit_code.is_some() && detail.summary.scrollback_bytes > 0 {
            break detail;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "session {id} never exited with output; exit_code={:?}, scrollback_bytes={}",
            detail.summary.exit_code,
            detail.summary.scrollback_bytes,
        );
        tokio::time::sleep(Duration::from_millis(40)).await;
    };
    let snapshot = base64::engine::general_purpose::STANDARD
        .decode(detail.scrollback_base64.as_bytes())
        .unwrap();
    String::from_utf8_lossy(&snapshot).into_owned()
}

async fn ws_attach(
    base: &str,
    id: &str,
) -> tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>> {
    ws_attach_with_token(base, id, TEST_TOKEN).await
}

async fn ws_attach_with_token(
    base: &str,
    id: &str,
    token: &str,
) -> tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>> {
    ws_attach_with_token_and_cursor(base, id, token, None).await
}

async fn ws_attach_with_token_and_cursor(
    base: &str,
    id: &str,
    token: &str,
    resume_from: Option<u64>,
) -> tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>> {
    let ws_url = base.replace("http://", "ws://");
    let url = format!("{ws_url}/api/sessions/{id}/attach");
    let (mut ws, _resp) = tokio_tungstenite::connect_async(url).await.unwrap();
    // First-frame auth (the legacy ?token= path still works but is deprecated).
    let auth = serde_json::json!({
        "type": "auth",
        "token": token,
        "resume_from": resume_from,
    })
    .to_string();
    ws.send(Message::Text(auth.into())).await.unwrap();
    ws
}

#[tokio::test]
async fn ws_attach_echoes_input_and_replays_on_reattach() {
    let (base, _h) = boot().await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    // `cat` echoes whatever we send it on stdin to stdout.
    let id: String = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({ "name": "echo", "command": ["/bin/cat"] }))
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    let mut ws = ws_attach(&base, &id).await;

    // 1) snapshot-start text frame
    let m = tokio::time::timeout(Duration::from_secs(2), ws.next())
        .await
        .expect("snapshot-start arrives")
        .unwrap()
        .unwrap();
    let s = match m {
        Message::Text(s) => s,
        other => panic!("expected text snapshot-start, got {other:?}"),
    };
    let v: Value = serde_json::from_str(&s).unwrap();
    assert_eq!(v["type"], "snapshot-start");

    // 2) snapshot-done (no binary frames in between since nothing has been written yet)
    let m = tokio::time::timeout(Duration::from_secs(2), ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let s = match m {
        Message::Text(s) => s,
        other => panic!("expected snapshot-done, got {other:?}"),
    };
    assert_eq!(
        serde_json::from_str::<Value>(&s).unwrap()["type"],
        "snapshot-done"
    );

    // 3) Write input → expect to see it echoed back.
    ws.send(Message::Binary(b"hello-mydevenv\n".to_vec().into()))
        .await
        .unwrap();

    let echoed = collect_binary_until(&mut ws, b"hello-mydevenv", Duration::from_secs(2)).await;
    assert!(
        echoed
            .windows(b"hello-mydevenv".len())
            .any(|w| w == b"hello-mydevenv"),
        "echo not seen; got {:?}",
        String::from_utf8_lossy(&echoed)
    );

    // Close first client.
    ws.close(None).await.ok();
    drop(ws);

    // 4) Reattach — snapshot must contain what we just echoed.
    let mut ws2 = ws_attach(&base, &id).await;
    let _start = ws2.next().await.unwrap().unwrap();
    let mut accumulated = Vec::new();
    loop {
        let m = tokio::time::timeout(Duration::from_secs(2), ws2.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        match m {
            Message::Binary(b) => accumulated.extend_from_slice(&b),
            Message::Text(s) => {
                let v: Value = serde_json::from_str(&s).unwrap();
                if v["type"] == "snapshot-done" {
                    break;
                }
            }
            _ => {}
        }
    }
    assert!(
        accumulated
            .windows(b"hello-mydevenv".len())
            .any(|w| w == b"hello-mydevenv"),
        "scrollback replay missing previous output; got {:?}",
        String::from_utf8_lossy(&accumulated)
    );

    let _ = client
        .delete(format!("{base}/api/sessions/{id}"))
        .send()
        .await;
}

#[tokio::test]
async fn ws_reattach_replays_only_output_after_a_valid_cursor() {
    let (base, _h) = boot().await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let id: String = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({
            "name": "resume",
            "command": ["/bin/sh", "-c", "stty -echo; exec /bin/cat"]
        }))
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    let mut ws = ws_attach(&base, &id).await;
    while let Some(Ok(message)) = ws.next().await {
        if let Message::Text(text) = message {
            if serde_json::from_str::<Value>(&text).unwrap()["type"] == "snapshot-done" {
                break;
            }
        }
    }
    ws.send(Message::Binary(b"before-cursor\n".to_vec().into()))
        .await
        .unwrap();
    let _ = collect_binary_until(&mut ws, b"before-cursor", Duration::from_secs(2)).await;

    let detail: Value = client
        .get(format!("{base}/api/sessions/{id}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let cursor = detail["scrollback_pos"].as_u64().unwrap();

    ws.send(Message::Binary(b"after-cursor\n".to_vec().into()))
        .await
        .unwrap();
    let _ = collect_binary_until(&mut ws, b"after-cursor", Duration::from_secs(2)).await;
    ws.close(None).await.ok();

    let mut resumed = ws_attach_with_token_and_cursor(&base, &id, TEST_TOKEN, Some(cursor)).await;
    let start = resumed.next().await.unwrap().unwrap();
    let start = match start {
        Message::Text(text) => serde_json::from_str::<Value>(&text).unwrap(),
        other => panic!("expected snapshot-start, got {other:?}"),
    };
    assert_eq!(start["type"], "snapshot-start");
    assert_eq!(start["reset"], false);

    let mut delta = Vec::new();
    loop {
        match resumed.next().await.unwrap().unwrap() {
            Message::Binary(bytes) => delta.extend_from_slice(&bytes),
            Message::Text(text)
                if serde_json::from_str::<Value>(&text).unwrap()["type"] == "snapshot-done" =>
            {
                break;
            }
            _ => {}
        }
    }
    assert!(delta
        .windows(b"after-cursor".len())
        .any(|w| w == b"after-cursor"));
    assert!(!delta
        .windows(b"before-cursor".len())
        .any(|w| w == b"before-cursor"));

    let _ = client
        .delete(format!("{base}/api/sessions/{id}"))
        .send()
        .await;
}

#[tokio::test]
async fn ws_rejects_oversized_input_without_closing_the_session() {
    let (base, _h) = boot().await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let id: String = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({
            "name": "bounded-input",
            "command": ["/bin/sh", "-c", "stty -echo; exec /bin/cat"]
        }))
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    let mut ws = ws_attach(&base, &id).await;
    while let Some(Ok(message)) = ws.next().await {
        if let Message::Text(text) = message {
            if serde_json::from_str::<Value>(&text).unwrap()["type"] == "snapshot-done" {
                break;
            }
        }
    }

    ws.send(Message::Binary(vec![b'x'; 64 * 1024 + 1].into()))
        .await
        .unwrap();
    ws.send(Message::Binary(b"still-responsive\n".to_vec().into()))
        .await
        .unwrap();
    let output = collect_binary_until(&mut ws, b"still-responsive", Duration::from_secs(2)).await;
    assert!(
        output
            .windows(b"still-responsive".len())
            .any(|window| window == b"still-responsive"),
        "normal input was not accepted after oversized frame"
    );
    assert!(
        !output
            .windows(128)
            .any(|window| window.iter().all(|byte| *byte == b'x')),
        "oversized input reached the PTY"
    );

    let _ = client
        .delete(format!("{base}/api/sessions/{id}"))
        .send()
        .await;
}

#[tokio::test]
async fn default_session_uses_agent_auth_helper_when_enabled() {
    let tmp = tempfile::tempdir().unwrap();
    let helper = tmp.path().join("agent-auth");
    std::fs::write(
        &helper,
        "#!/bin/sh\n[ \"$1\" = shell ] || exit 64\nprintf 'agent-wrapper-ok\\n'\nexec /bin/cat\n",
    )
    .unwrap();
    let mut permissions = std::fs::metadata(&helper).unwrap().permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(&helper, permissions).unwrap();

    let mut cfg = test_config();
    cfg.default_cwd = tmp.path().to_path_buf();
    cfg.workspace_root = tmp.path().canonicalize().unwrap();
    cfg.auto_agent_auth = true;
    cfg.agent_auth_helper = helper;

    let (base, _h) = boot_with_config(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let id = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({ "name": "authenticated-shell" }))
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    let mut ws = ws_attach(&base, &id).await;
    let output = collect_binary_until(&mut ws, b"agent-wrapper-ok", Duration::from_secs(2)).await;
    assert!(
        output
            .windows(b"agent-wrapper-ok".len())
            .any(|w| w == b"agent-wrapper-ok"),
        "agent auth helper output not seen; got {:?}",
        String::from_utf8_lossy(&output)
    );

    let _ = client
        .delete(format!("{base}/api/sessions/{id}"))
        .send()
        .await;
}

#[tokio::test]
async fn file_api_round_trip() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("hello.txt"), "first").unwrap();
    std::fs::create_dir(tmp.path().join("sub")).unwrap();
    std::fs::write(tmp.path().join("sub/nested.md"), "# nested").unwrap();

    let cfg = Config {
        default_cwd: tmp.path().to_path_buf(),
        workspace_root: tmp.path().canonicalize().unwrap(),
        ..test_config()
    };
    let (router, _state) = router(cfg).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    let base = format!("http://{addr}");
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    // List root
    let dir: Vec<Value> = client
        .get(format!("{base}/api/dir?path="))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let names: Vec<&str> = dir.iter().filter_map(|e| e["name"].as_str()).collect();
    assert!(names.contains(&"hello.txt"));
    assert!(names.contains(&"sub"));

    // Read existing file
    let r: Value = client
        .get(format!("{base}/api/files?path=hello.txt"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(r["content"], "first");

    // Write a new file
    let w = client
        .put(format!("{base}/api/files"))
        .json(&json!({ "path": "new.txt", "content": "fresh" }))
        .send()
        .await
        .unwrap();
    assert_eq!(w.status(), StatusCode::OK);
    let bytes_on_disk = std::fs::read_to_string(tmp.path().join("new.txt")).unwrap();
    assert_eq!(bytes_on_disk, "fresh");

    // Create a directory via higher-level file ops.
    let mkdir = client
        .post(format!("{base}/api/files/op"))
        .json(&json!({ "op": "mkdir", "path": "ops/deeper", "parents": true }))
        .send()
        .await
        .unwrap();
    assert_eq!(mkdir.status(), StatusCode::OK);
    assert!(tmp.path().join("ops/deeper").is_dir());

    // Upload binary bytes via content_base64 (native client upload path).
    use base64::Engine as _;
    let raw: &[u8] = &[0x00, 0x01, 0xff, 0xfe, b'h', b'i'];
    let b64 = base64::engine::general_purpose::STANDARD.encode(raw);
    let wb = client
        .put(format!("{base}/api/files"))
        .json(&json!({ "path": "up/bin.dat", "content_base64": b64, "create_parents": true }))
        .send()
        .await
        .unwrap();
    assert_eq!(wb.status(), StatusCode::OK);
    let on_disk = std::fs::read(tmp.path().join("up/bin.dat")).unwrap();
    assert_eq!(on_disk, raw);

    // Duplicate both a file and a directory tree.
    let dup_file = client
        .post(format!("{base}/api/files/op"))
        .json(&json!({
            "op": "duplicate",
            "from": "hello.txt",
            "to": "ops/hello-copy.txt",
            "create_parents": true
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(dup_file.status(), StatusCode::OK);
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("ops/hello-copy.txt")).unwrap(),
        "first"
    );

    let dup_dir = client
        .post(format!("{base}/api/files/op"))
        .json(&json!({
            "op": "duplicate",
            "from": "sub",
            "to": "sub-copy"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(dup_dir.status(), StatusCode::OK);
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("sub-copy/nested.md")).unwrap(),
        "# nested"
    );

    // Move/rename a file.
    let mv = client
        .post(format!("{base}/api/files/op"))
        .json(&json!({
            "op": "move",
            "from": "new.txt",
            "to": "ops/renamed.txt",
            "create_parents": true
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(mv.status(), StatusCode::OK);
    assert!(!tmp.path().join("new.txt").exists());
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("ops/renamed.txt")).unwrap(),
        "fresh"
    );

    // Delete a copied file and directory tree.
    let del_file = client
        .post(format!("{base}/api/files/op"))
        .json(&json!({ "op": "delete", "path": "ops/hello-copy.txt" }))
        .send()
        .await
        .unwrap();
    assert_eq!(del_file.status(), StatusCode::OK);
    assert!(!tmp.path().join("ops/hello-copy.txt").exists());

    let del_dir = client
        .post(format!("{base}/api/files/op"))
        .json(&json!({ "op": "delete", "path": "sub-copy", "recursive": true }))
        .send()
        .await
        .unwrap();
    assert_eq!(del_dir.status(), StatusCode::OK);
    assert!(!tmp.path().join("sub-copy").exists());

    // A malformed base64 body is a 400, not a 500.
    let bad = client
        .put(format!("{base}/api/files"))
        .json(&json!({ "path": "bad.dat", "content_base64": "!!!notbase64!!!" }))
        .send()
        .await
        .unwrap();
    assert_eq!(bad.status(), StatusCode::BAD_REQUEST);

    // Path-traversal rejected
    let escape = client
        .get(format!("{base}/api/files?path=../escape"))
        .send()
        .await
        .unwrap();
    assert_eq!(escape.status(), StatusCode::BAD_REQUEST);

    // Tree with depth 1 includes nested.md
    let tree: Vec<Value> = client
        .get(format!("{base}/api/tree?depth=1"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let sub = tree
        .iter()
        .find(|n| n["name"] == "sub")
        .expect("sub node present");
    let kids: Vec<&str> = sub["children"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|c| c["name"].as_str())
        .collect();
    assert!(kids.contains(&"nested.md"));

    // Search (skip if rg isn't installed; just don't fail the suite)
    let s = client
        .get(format!("{base}/api/search?q=nested"))
        .send()
        .await
        .unwrap();
    if s.status() == StatusCode::OK {
        let hits: Vec<Value> = s.json().await.unwrap();
        assert!(
            hits.iter().any(|h| h["path"]
                .as_str()
                .map(|p| p.ends_with("nested.md"))
                .unwrap_or(false)),
            "rg search should find nested; got {hits:?}"
        );
    }
}

#[tokio::test]
async fn read_returns_hash_and_mtime_and_if_match_guards_writes() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("note.txt"), "original").unwrap();

    let cfg = Config {
        default_cwd: tmp.path().to_path_buf(),
        workspace_root: tmp.path().canonicalize().unwrap(),
        ..test_config()
    };
    let (router, _state) = router(cfg).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    let base = format!("http://{addr}");
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    // Read exposes a content hash and an mtime.
    let r: Value = client
        .get(format!("{base}/api/files?path=note.txt"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(r["content"], "original");
    let hash = r["hash"].as_str().expect("read returns a hash").to_string();
    assert_eq!(hash.len(), 64, "sha-256 hex is 64 chars");
    assert!(r["mtime"].as_u64().unwrap() > 0, "read returns an mtime");

    // Happy path: writing with the matching if_match succeeds and hands back a
    // fresh baseline hash/mtime.
    let w = client
        .put(format!("{base}/api/files"))
        .json(&json!({ "path": "note.txt", "content": "edited by me", "if_match": hash }))
        .send()
        .await
        .unwrap();
    assert_eq!(w.status(), StatusCode::OK);
    let wbody: Value = w.json().await.unwrap();
    let new_hash = wbody["hash"].as_str().expect("write returns new hash");
    assert_ne!(new_hash, hash, "baseline advanced after the write");
    assert!(wbody["mtime"].as_u64().unwrap() > 0);
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("note.txt")).unwrap(),
        "edited by me"
    );

    // Simulate an external change on disk, then attempt to save against the now
    // stale baseline: the write is refused with 409 Conflict and the file is
    // left untouched.
    std::fs::write(tmp.path().join("note.txt"), "changed underfoot").unwrap();
    let stale = client
        .put(format!("{base}/api/files"))
        .json(&json!({ "path": "note.txt", "content": "my clobber", "if_match": new_hash }))
        .send()
        .await
        .unwrap();
    assert_eq!(stale.status(), StatusCode::CONFLICT);
    let body: Value = stale.json().await.unwrap();
    assert!(
        body["error"].as_str().unwrap().contains("changed on disk"),
        "conflict body should explain the staleness; got {body:?}"
    );
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("note.txt")).unwrap(),
        "changed underfoot",
        "a conflicting write must not clobber the newer on-disk content"
    );

    // Without if_match the write still wins unconditionally (legacy behaviour).
    let force = client
        .put(format!("{base}/api/files"))
        .json(&json!({ "path": "note.txt", "content": "forced" }))
        .send()
        .await
        .unwrap();
    assert_eq!(force.status(), StatusCode::OK);
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("note.txt")).unwrap(),
        "forced"
    );
}

#[tokio::test]
async fn file_name_search_returns_matching_paths() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir(tmp.path().join("docs")).unwrap();
    std::fs::write(tmp.path().join("README.md"), "root").unwrap();
    std::fs::write(tmp.path().join("docs").join("readme-notes.txt"), "nested").unwrap();
    std::fs::write(tmp.path().join("docs").join("other.txt"), "other").unwrap();

    let mut cfg = test_config();
    cfg.default_cwd = tmp.path().to_path_buf();
    cfg.workspace_root = tmp.path().canonicalize().unwrap();

    let (base, _h) = boot_with_config(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    let hits: Vec<Value> = client
        .get(format!("{base}/api/search/files?q=read"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert!(hits.iter().any(|hit| hit["path"] == "README.md"));
    assert!(hits
        .iter()
        .any(|hit| hit["path"] == "docs/readme-notes.txt"));
}

#[tokio::test]
async fn git_status_log_branch() {
    // Spin up a fresh git repo in a tempdir as the workspace, then drive
    // the git API across status, diff, staging, commit, and branch workflow.
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    let sh = |cmd: &str| {
        let out = std::process::Command::new("sh")
            .arg("-c")
            .arg(cmd)
            .current_dir(repo)
            .output()
            .unwrap();
        assert!(out.status.success(), "{cmd}: {:?}", out);
    };
    sh("git init -q -b main");
    sh("git config user.email t@t");
    sh("git config user.name t");
    std::fs::write(repo.join("a.txt"), "one\n").unwrap();
    sh("git add a.txt && git commit -q -m 'init'");
    std::fs::write(repo.join("a.txt"), "one\ntwo\n").unwrap();
    std::fs::write(repo.join("b.txt"), "untracked\n").unwrap();

    let cfg = Config {
        default_cwd: repo.to_path_buf(),
        workspace_root: repo.canonicalize().unwrap(),
        ..test_config()
    };
    let (router, _state) = router(cfg).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    let base = format!("http://{addr}");
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    let st: Value = client
        .get(format!("{base}/api/git/status"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(st["is_repo"], true);
    assert_eq!(st["branch"], "main");
    let entries = st["entries"].as_array().unwrap();
    let paths: Vec<&str> = entries.iter().filter_map(|e| e["path"].as_str()).collect();
    assert!(paths.contains(&"a.txt"));
    assert!(paths.contains(&"b.txt"));

    let log: Vec<Value> = client
        .get(format!("{base}/api/git/log?n=10"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(log.len(), 1);
    assert_eq!(log[0]["subject"], "init");

    let br: Value = client
        .get(format!("{base}/api/git/branch"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(br["current"], "main");

    let diff: Value = client
        .get(format!("{base}/api/git/diff?path=a.txt"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(diff["head"], "one\n");
    assert_eq!(diff["current"], "one\ntwo\n");

    let stage_a: Value = client
        .post(format!("{base}/api/git/op"))
        .json(&json!({
            "op": "stage",
            "path": "a.txt",
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(stage_a["ok"], true);

    let staged_status: Value = client
        .get(format!("{base}/api/git/status"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let staged_a = staged_status["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["path"] == "a.txt")
        .unwrap();
    assert_eq!(staged_a["kind"], "staged");
    assert_eq!(staged_a["index"], "M");
    assert_eq!(staged_a["worktree"], " ");

    let unstage_a: Value = client
        .post(format!("{base}/api/git/op"))
        .json(&json!({
            "op": "unstage",
            "path": "a.txt",
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(unstage_a["ok"], true);

    let discard_a: Value = client
        .post(format!("{base}/api/git/op"))
        .json(&json!({
            "op": "discard",
            "path": "a.txt",
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(discard_a["ok"], true);
    assert_eq!(
        std::fs::read_to_string(repo.join("a.txt")).unwrap(),
        "one\n"
    );

    let stage_b: Value = client
        .post(format!("{base}/api/git/op"))
        .json(&json!({
            "op": "stage",
            "path": "b.txt",
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(stage_b["ok"], true);

    let unstage_b: Value = client
        .post(format!("{base}/api/git/op"))
        .json(&json!({
            "op": "unstage",
            "path": "b.txt",
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(unstage_b["ok"], true);

    let discard_b: Value = client
        .post(format!("{base}/api/git/op"))
        .json(&json!({
            "op": "discard",
            "path": "b.txt",
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(discard_b["ok"], true);
    assert!(!repo.join("b.txt").exists());

    std::fs::write(repo.join("b.txt"), "tracked now\n").unwrap();
    let stage_commit_target: Value = client
        .post(format!("{base}/api/git/op"))
        .json(&json!({
            "op": "stage",
            "path": "b.txt",
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(stage_commit_target["ok"], true);

    let commit: Value = client
        .post(format!("{base}/api/git/op"))
        .json(&json!({
            "op": "commit",
            "message": "add b",
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(commit["ok"], true);
    assert!(commit["commit"].as_str().unwrap().len() >= 7);

    let clean_status: Value = client
        .get(format!("{base}/api/git/status"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(clean_status["entries"].as_array().unwrap().is_empty());

    let updated_log: Vec<Value> = client
        .get(format!("{base}/api/git/log?n=10"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(updated_log.len(), 2);
    assert_eq!(updated_log[0]["subject"], "add b");

    let create_branch: Value = client
        .post(format!("{base}/api/git/op"))
        .json(&json!({
            "op": "checkout",
            "branch": "feature/git-workflow",
            "create": true,
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(create_branch["ok"], true);
    assert_eq!(create_branch["branch"], "feature/git-workflow");

    let branch_after_create: Value = client
        .get(format!("{base}/api/git/branch"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(branch_after_create["current"], "feature/git-workflow");
    let all_branches = branch_after_create["all"].as_array().unwrap();
    assert!(all_branches
        .iter()
        .any(|branch| branch.as_str() == Some("feature/git-workflow")));

    let checkout_main: Value = client
        .post(format!("{base}/api/git/op"))
        .json(&json!({
            "op": "checkout",
            "branch": "main",
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(checkout_main["ok"], true);
    assert_eq!(checkout_main["branch"], "main");

    let branch_after_checkout: Value = client
        .get(format!("{base}/api/git/branch"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(branch_after_checkout["current"], "main");
}

#[tokio::test]
async fn git_endpoints_return_empty_for_non_repo_workspace() {
    let tmp = tempfile::tempdir().unwrap();
    // Reproduce the deployed workspace shape from vogt#53: an empty `.git`
    // placeholder is not a repository and must not make status return 500.
    std::fs::create_dir(tmp.path().join(".git")).unwrap();
    std::fs::create_dir(tmp.path().join("plain-dir")).unwrap();

    let mut cfg = test_config();
    cfg.default_cwd = tmp.path().to_path_buf();
    cfg.workspace_root = tmp.path().canonicalize().unwrap();

    let (base, _h) = boot_with_config(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    let response = client
        .get(format!("{base}/api/git/status"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let st: Value = response.json().await.unwrap();
    assert_eq!(st["is_repo"], false);
    assert_eq!(st["branch"], "");
    assert!(st["entries"].as_array().unwrap().is_empty());

    let nested_status: Value = client
        .get(format!("{base}/api/git/status?repo=plain-dir"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(nested_status["repo"], "plain-dir");
    assert_eq!(nested_status["is_repo"], false);
    assert!(nested_status["entries"].as_array().unwrap().is_empty());

    let nested_log: Vec<Value> = client
        .get(format!("{base}/api/git/log?repo=plain-dir&n=10"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(nested_log.is_empty());

    let nested_branch: Value = client
        .get(format!("{base}/api/git/branch?repo=plain-dir"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(nested_branch["current"], "");
    assert!(nested_branch["all"].as_array().unwrap().is_empty());

    let log: Vec<Value> = client
        .get(format!("{base}/api/git/log?n=10"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(log.is_empty());

    let br: Value = client
        .get(format!("{base}/api/git/branch"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(br["current"], "");
    assert!(br["all"].as_array().unwrap().is_empty());

    // Read endpoints deliberately render an empty non-repository state.
    // Mutation must still refuse: returning success here would claim a Git
    // effect happened in an ordinary directory.
    let operation = client
        .post(format!("{base}/api/git/operate"))
        .json(&serde_json::json!({
            "op": "stage",
            "repo": "plain-dir",
            "path": "nothing.txt"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(operation.status(), StatusCode::NOT_FOUND);
}

async fn collect_binary_until(
    ws: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    needle: &[u8],
    timeout: Duration,
) -> Vec<u8> {
    let deadline = tokio::time::Instant::now() + timeout;
    let mut buf = Vec::new();
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return buf;
        }
        let msg = match tokio::time::timeout(remaining, ws.next()).await {
            Ok(Some(Ok(m))) => m,
            _ => return buf,
        };
        if let Message::Binary(b) = msg {
            buf.extend_from_slice(&b);
            if buf.windows(needle.len()).any(|w| w == needle) {
                return buf;
            }
        }
    }
}

// ── FR-T7: a configuration that would hang refuses instead ────────────────

#[tokio::test]
async fn a_claude_route_on_the_openai_backend_refuses_with_a_named_reason() {
    // The recorded failure is that these proxy routes accept the request and
    // never answer. Under a 60-second client timeout that reads as "the
    // request took too long", which is a different sentence for the same
    // silence and sends an operator looking in the wrong place.
    let mut cfg = test_config();
    cfg.assistant_api_key = Some("sk-test".into());
    cfg.assistant_model = "claude-sonnet-4-5".into();
    let (base, _h) = boot_with_config(cfg).await;

    let res = reqwest::Client::new()
        .get(format!("{base}/api/assistant/history"))
        .headers(auth())
        .send()
        .await
        .unwrap();
    // Not 404: the assistant *is* provisioned, and reporting it absent would
    // send somebody looking for a missing API key.
    assert_ne!(res.status(), reqwest::StatusCode::NOT_FOUND);
    let body = res.text().await.unwrap();
    assert!(body.contains("claude-sonnet-4-5"), "{body}");
    assert!(body.contains("assistant_allow_claude_proxy"), "{body}");
}

#[tokio::test]
async fn the_assistant_answers_normally_for_a_model_this_transport_serves() {
    let mut cfg = test_config();
    cfg.assistant_api_key = Some("sk-test".into());
    let (base, _h) = boot_with_config(cfg).await;

    let res = reqwest::Client::new()
        .get(format!("{base}/api/assistant/history"))
        .headers(auth())
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), reqwest::StatusCode::OK);
}

// ── FR-T6: unprovisioned means invisible, not broken ──────────────────────

#[tokio::test]
async fn without_an_api_key_every_assistant_route_is_absent() {
    // The feature is invisible unless provisioned: a 404 rather than a 500 or
    // an empty transcript, so a deployment that never configured an assistant
    // does not look like one whose assistant is broken. Asserted here because
    // every other test in this file boots with `assistant_api_key: None` and
    // simply never asks.
    let (base, _h) = boot().await;
    let client = reqwest::Client::new();
    for (method, path) in [
        ("GET", "/api/assistant/history"),
        ("POST", "/api/assistant/message"),
        ("POST", "/api/assistant/reset"),
    ] {
        let req = match method {
            "GET" => client.get(format!("{base}{path}")),
            _ => client
                .post(format!("{base}{path}"))
                .json(&serde_json::json!({"text": "hello"})),
        };
        let res = req.headers(auth()).send().await.unwrap();
        assert_eq!(
            res.status(),
            reqwest::StatusCode::NOT_FOUND,
            "{method} {path} should be absent, not broken"
        );
    }
}

// ── FR-E9: sessions do not depend on the core ─────────────────────────────

#[tokio::test]
async fn sessions_work_with_no_vogt_core_configured() {
    // This is exercised by every session test in this file, because they all
    // boot with `vogt_core_url: None` — and that is exactly why it needed
    // naming. A reader looking for the requirement found nothing, and the day
    // somebody gives this fixture a core, the coverage would vanish without a
    // single test turning red.
    let cfg = test_config();
    assert!(
        cfg.vogt_core_url.is_none(),
        "this test is about the core being absent; if the fixture gains one, \
         FR-E9 needs its own fixture rather than this assertion deleted"
    );
    let (base, _h) = boot_with_config(cfg).await;
    let client = reqwest::Client::new();

    let created = client
        .post(format!("{base}/api/sessions"))
        .headers(auth())
        .json(&serde_json::json!({"name": "no-core"}))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), reqwest::StatusCode::OK);
    let id = created.json::<serde_json::Value>().await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    let listed = client
        .get(format!("{base}/api/sessions"))
        .headers(auth())
        .send()
        .await
        .unwrap();
    assert_eq!(listed.status(), reqwest::StatusCode::OK);

    // And the container is ready, so a healthcheck does not restart the
    // engine — which could not revive a core and would kill every live PTY
    // trying.
    let ready = client.get(format!("{base}/readyz")).send().await.unwrap();
    assert!(ready.status().is_success());

    let killed = client
        .post(format!("{base}/api/sessions/{id}/kill"))
        .headers(auth())
        .send()
        .await
        .unwrap();
    assert_eq!(killed.status(), reqwest::StatusCode::OK);
}

// ── FR-E1: more than one client on one session, at the same time ──────────

#[tokio::test]
async fn two_clients_watch_one_session_at_once() {
    // "Multiple concurrent clients per session" is the conjunct, and the
    // existing multi-attach test closes the first socket before opening the
    // second — which exercises re-attachment, not concurrency. The difference
    // matters: a second attach that silently displaced the first would pass
    // that test and lose somebody's terminal.
    let (base, _h) = boot().await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    let id: String = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({ "name": "shared", "command": ["/bin/cat"] }))
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    let mut first = ws_attach(&base, &id).await;
    // Bounded, because the failure this is looking for is a *hang*: if the
    // server cannot accept a second socket while the first is attached, the
    // upgrade never completes and an unbounded await would take the suite
    // down with it rather than reporting.
    let mut second = tokio::time::timeout(Duration::from_secs(10), ws_attach(&base, &id))
        .await
        .expect("a second client must be able to attach while the first is attached");

    // Both are still attached, so both see the same output — the first is
    // asserted *after* the second connected, which is the whole point.
    first
        .send(Message::Binary(b"shared-line\n".to_vec().into()))
        .await
        .unwrap();

    let contains =
        |haystack: &[u8], needle: &[u8]| haystack.windows(needle.len()).any(|w| w == needle);
    let seen_by_first =
        collect_binary_until(&mut first, b"shared-line", Duration::from_secs(3)).await;
    let seen_by_second =
        collect_binary_until(&mut second, b"shared-line", Duration::from_secs(3)).await;
    assert!(
        contains(&seen_by_first, b"shared-line"),
        "the client that typed it must still be attached"
    );
    assert!(
        contains(&seen_by_second, b"shared-line"),
        "the second client must see output caused by the first — one PTY, two \
         watchers, which is what FR-E1 means by concurrent"
    );

    // And the second can type too: attaching is not read-only for whoever
    // arrived later.
    second
        .send(Message::Binary(b"from-the-second\n".to_vec().into()))
        .await
        .unwrap();
    let back = collect_binary_until(&mut first, b"from-the-second", Duration::from_secs(3)).await;
    assert!(
        contains(&back, b"from-the-second"),
        "the first client must see what the second typed"
    );

    // Close both sockets before the session goes. Every assertion above
    // passes without this and the test then hangs on teardown, which is worth
    // recording because a hang after the last assertion reads exactly like a
    // failure of the thing being tested — it had somebody looking for a
    // concurrency defect in the engine that was never there.
    //
    // The cause is ordinary refcounting, and it is why two sockets differ
    // from one: a socket's handler ends when its client disconnects or the
    // session's broadcast closes, and that broadcast cannot close while a
    // handler is holding the session alive. Killing the child is not enough;
    // the clients have to leave.
    let _ = first.close(None).await;
    let _ = second.close(None).await;
    let _ = client
        .delete(format!("{base}/api/sessions/{id}"))
        .send()
        .await;
}

// ── FR-E2: the activity state reaches the server-wide event stream ────────

/// Read the SSE stream until an event satisfies `want`, or give up.
///
/// SSE frames are `data: <json>` lines; keep-alive comments start with `:`
/// and are skipped. A chunk boundary can fall anywhere, so the partial line
/// is carried across reads rather than assumed to end on one.
async fn event_matching(
    stream: &mut (impl futures_util::Stream<Item = reqwest::Result<bytes::Bytes>> + Unpin),
    want: impl Fn(&Value) -> bool,
) -> Value {
    let mut partial = String::new();
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let chunk = stream
                .next()
                .await
                .expect("the event stream ended")
                .expect("the event stream failed");
            partial.push_str(&String::from_utf8_lossy(&chunk));
            while let Some(idx) = partial.find('\n') {
                let line: String = partial.drain(..=idx).collect();
                let Some(raw) = line.trim_end().strip_prefix("data: ") else {
                    continue;
                };
                let Ok(event) = serde_json::from_str::<Value>(raw) else {
                    continue;
                };
                if want(&event) {
                    return event;
                }
            }
        }
    })
    .await
    .expect("no matching event arrived on /api/events")
}

#[tokio::test]
async fn the_activity_state_is_announced_on_the_server_wide_event_stream() {
    // FR-E2 has two halves — the state is *derived from output heuristics*,
    // and it is *published on the server-wide SSE stream*. `activity.rs` owns
    // the first and asserts it four ways. The second was asserted by nothing:
    // the one activity test in this file polls `GET /api/sessions/{id}`, and
    // a bus publish that stopped happening would leave that test green and
    // every client on the stream showing a stale badge until it refreshed.
    let (base, _h) = boot().await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    // Subscribe first. The stream is a live broadcast rather than a log, so a
    // reader that opens after the session has already spoken sees nothing and
    // would fail this test for the wrong reason.
    let stream_res = client
        .get(format!("{base}/api/events"))
        .send()
        .await
        .unwrap();
    assert_eq!(stream_res.status(), StatusCode::OK);
    assert_eq!(
        stream_res
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(|value| value.split(';').next().unwrap_or(value).trim().to_string()),
        Some("text/event-stream".to_string())
    );
    let mut stream = stream_res.bytes_stream();

    // A prompt the heuristics recognise, then a wait — so the state the
    // stream carries is one `activity.rs` derived from output rather than a
    // lifecycle state every session passes through.
    let created: Value = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({
            "name": "sse-activity",
            "command": ["/bin/sh", "-lc", "printf 'Continue? [y/N]'; sleep 30"],
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = created["id"].as_str().unwrap().to_string();

    let want_id = id.clone();
    let event = event_matching(&mut stream, move |event| {
        event["type"] == "activity"
            && event["id"] == want_id.as_str()
            && event["state"] == "waiting-for-input"
    })
    .await;
    assert_eq!(
        event["state"], "waiting-for-input",
        "the first activity this session announced was not the state its \
         output implies: {event}"
    );

    // And the stream and the polled detail are the same fact, not two. A
    // stream that announced a state the session does not hold would be worse
    // than one that announced nothing.
    let detail: Value = client
        .get(format!("{base}/api/sessions/{id}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(detail["summary"]["activity"], event["state"]);

    // Let go of the stream before the session goes: the handler holds a
    // subscriber until its client leaves.
    drop(stream);
    let _ = client
        .delete(format!("{base}/api/sessions/{id}"))
        .send()
        .await;
}

// ── FR-M2: the notifications that are worth a phone interruption ──────────

/// Deliveries a stand-in push service received, by the path they arrived on.
type PushLog = Arc<Mutex<Vec<String>>>;

/// A stand-in push service — an HTTP server that records the path of every
/// delivery, and nothing else.
///
/// It records the path because it cannot record anything better: a Web Push
/// body is encrypted to the subscription's key, so what a delivery *says* is
/// unreadable from here by design. Each subscription in the tests below
/// therefore gets an endpoint of its own and preferences that admit exactly
/// one `NotificationKind`, which makes the path that received a POST the kind
/// that was routed — and makes the endpoint that stayed empty an assertion
/// that the wrong kind was not.
async fn start_stand_in_push_service() -> (PushLog, String) {
    use axum::{extract::State, routing::post, Router};

    async fn record(State(log): State<PushLog>, uri: axum::http::Uri) -> &'static str {
        log.lock().unwrap().push(uri.path().to_string());
        ""
    }

    let log: PushLog = Arc::new(Mutex::new(Vec::new()));
    let app = Router::new()
        .route("/{*rest}", post(record))
        .with_state(Arc::clone(&log));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    (log, format!("http://{addr}"))
}

/// A real P-256 subscription keypair. The engine encrypts to it for real —
/// an invalid key would fail inside `web-push` and never reach the wire, so
/// a test with a made-up one would be asserting that nothing was sent.
fn web_push_keys() -> (String, String) {
    let rng = ring::rand::SystemRandom::new();
    let private =
        ring::agreement::EphemeralPrivateKey::generate(&ring::agreement::ECDH_P256, &rng).unwrap();
    let public = private.compute_public_key().unwrap();
    let mut auth = [0u8; 16];
    ring::rand::SecureRandom::fill(&rng, &mut auth).unwrap();
    let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    (b64.encode(public.as_ref()), b64.encode(auth))
}

/// Preferences that admit one kind and refuse the other five. Written out in
/// full rather than partially, because every one of these fields defaults to
/// the value FR-M2 gives it and an omitted `errored` would silently be `true`.
fn admitting_only(kind: &str) -> Value {
    let mut prefs = json!({
        "waiting_for_input": false,
        "errored": false,
        "idle_stall": false,
        "agent_task_started": false,
        "agent_task_notify": false,
        "drift": false,
    });
    prefs[kind] = json!(true);
    prefs
}

/// Register a device that will accept exactly one kind of interruption.
async fn subscribe_for_kind(client: &reqwest::Client, base: &str, push_base: &str, kind: &str) {
    let (p256dh, auth_secret) = web_push_keys();
    let subscribed: Value = client
        .post(format!("{base}/api/push/subscribe"))
        .json(&json!({
            "kind": "web-push",
            "endpoint": format!("{push_base}/{kind}"),
            "p256dh": p256dh,
            "auth": auth_secret,
            "label": kind,
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = subscribed["id"].as_str().expect("a subscription id");
    let updated: Value = client
        .post(format!("{base}/api/push/update"))
        .json(&json!({ "id": id, "prefs": admitting_only(kind) }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(updated["ok"], true, "{updated}");
    assert_eq!(updated["prefs"][kind], true, "{updated}");
}

/// Wait for a delivery on `path`, and report what the whole log holds if none
/// arrives — a bare timeout would say only that the wait ended.
async fn delivered_to(log: &PushLog, path: &str) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        if log.lock().unwrap().iter().any(|seen| seen == path) {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "nothing was pushed to {path}; the service saw {:?}",
            log.lock().unwrap()
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn nothing_delivered_to(log: &PushLog, path: &str) {
    let seen = log.lock().unwrap();
    assert!(
        !seen.iter().any(|got| got == path),
        "a notification was routed to {path}, which asked for a different \
         kind entirely; the service saw {seen:?}"
    );
}

#[tokio::test]
async fn a_session_that_starts_waiting_for_input_wakes_a_phone() {
    // FR-M2's headline case, and the one the drift watcher's unit tests do
    // not touch: `spawn_activity_watcher` reads the bus and turns a state
    // change into a push. Driven end to end because the routing is the
    // requirement — a watcher that stopped subscribing, a `notify` that lost
    // its kind, or a preference that stopped meaning what it says would each
    // leave a phone silent, and none of them is visible from inside the
    // heuristic that produced the state.
    let (log, push_base) = start_stand_in_push_service().await;
    let (base, _h) = boot().await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    subscribe_for_kind(&client, &base, &push_base, "waiting_for_input").await;
    subscribe_for_kind(&client, &base, &push_base, "errored").await;

    let created: Value = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({
            "name": "asks-a-question",
            "command": ["/bin/sh", "-lc", "printf 'Continue? [y/N]'; sleep 30"],
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = created["id"].as_str().unwrap().to_string();

    delivered_to(&log, "/waiting_for_input").await;
    // The session is alive and waiting, so nothing has errored — and the
    // device that only asked about errors must not have been woken.
    nothing_delivered_to(&log, "/errored");

    // Killing it here would error the session and push again, so every
    // assertion is made before the cleanup rather than after it.
    let _ = client
        .delete(format!("{base}/api/sessions/{id}"))
        .send()
        .await;
}

#[tokio::test]
async fn a_session_that_exits_badly_wakes_a_phone() {
    let (log, push_base) = start_stand_in_push_service().await;
    let (base, _h) = boot().await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    subscribe_for_kind(&client, &base, &push_base, "waiting_for_input").await;
    subscribe_for_kind(&client, &base, &push_base, "errored").await;

    let created: Value = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({
            "name": "falls-over",
            "command": ["/bin/sh", "-lc", "printf 'it went wrong\\n'; exit 3"],
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = created["id"].as_str().unwrap().to_string();

    delivered_to(&log, "/errored").await;
    // Nothing about that output looks like a prompt, so the other device
    // stays quiet: the two are routed by the state a session reached, not by
    // the fact that something happened to it.
    nothing_delivered_to(&log, "/waiting_for_input");

    let _ = client
        .delete(format!("{base}/api/sessions/{id}"))
        .send()
        .await;
}

#[tokio::test]
async fn the_agent_task_notify_hook_wakes_a_phone() {
    // The third of FR-M2's named events, and the only one that comes from a
    // task rather than from a session's state. A run's finding is asserted
    // elsewhere; that the finding also *interrupts somebody* is this, and it
    // is the half an unattended task exists for.
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = test_config();
    cfg.default_cwd = tmp.path().to_path_buf();
    cfg.workspace_root = tmp.path().canonicalize().unwrap();
    cfg.state_dir = tmp.path().join("state");

    let (log, push_base) = start_stand_in_push_service().await;
    let (base, _h) = boot_with_config(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    subscribe_for_kind(&client, &base, &push_base, "agent_task_notify").await;
    subscribe_for_kind(&client, &base, &push_base, "waiting_for_input").await;

    let created: Value = client
        .post(format!("{base}/api/agent-tasks"))
        .json(&json!({
            "name": "Nightly sweep",
            "prompt": "Look for unresolved internal references.",
            "schedule": { "kind": "manual" },
            // The sleep is not decoration: the watcher subscribes just after
            // the session is created, and a `printf` that finished first
            // would be a race rather than a test.
            "command": ["/bin/sh", "-lc",
                "sleep 0.3; printf 'MYDEVENV2_NOTIFY: two references are dangling\\n'"],
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let task_id = created["id"].as_str().unwrap().to_string();

    let run = client
        .post(format!("{base}/api/agent-tasks/{task_id}/run"))
        .send()
        .await
        .unwrap();
    assert_eq!(run.status(), StatusCode::OK);

    delivered_to(&log, "/agent_task_notify").await;
    // The task's session never asked a question, so the device watching for
    // that was not woken — the hook is routed as its own kind.
    nothing_delivered_to(&log, "/waiting_for_input");
}

// -- Server-side speech (FR-T12) --------------------------------------------

/// Unconfigured, both speech routes 404 so the client falls back (FR-T6). The
/// request is well-formed — a real multipart upload and a real JSON body — so
/// the 404 is the handler's "this half is not provisioned", not an extractor
/// rejecting a malformed request.
#[tokio::test]
async fn server_speech_routes_404_when_unconfigured() {
    let (base, _h) = boot().await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    let form = reqwest::multipart::Form::new()
        .text("model", "whisper-1")
        .part(
            "file",
            reqwest::multipart::Part::bytes(vec![0u8, 1, 2, 3])
                .file_name("take.webm")
                .mime_str("audio/webm")
                .unwrap(),
        );
    let stt = client
        .post(format!("{base}/api/assistant/stt"))
        .multipart(form)
        .send()
        .await
        .unwrap();
    assert_eq!(stt.status(), StatusCode::NOT_FOUND);

    let tts = client
        .post(format!("{base}/api/assistant/tts"))
        .json(&json!({ "text": "hello" }))
        .send()
        .await
        .unwrap();
    assert_eq!(tts.status(), StatusCode::NOT_FOUND);
}

/// Both speech routes require the `assistant` capability, enforced by the
/// `starts_with("/api/assistant") && != GET` rule in `auth::required_capability`
/// — a token without it is refused before the handler runs, so a speech route
/// is never an ungated door into the assistant. Asserted end-to-end here in
/// addition to the unit test in `auth.rs`.
#[tokio::test]
async fn server_speech_routes_require_the_assistant_capability() {
    use vogt_engine_server::auth::{ScopedTokenConfig, TokenCapability};

    let mut cfg = test_config();
    // A token that may do sessions but not assistant.
    cfg.extra_tokens = vec![ScopedTokenConfig {
        name: "no-assistant".into(),
        token: "no-assistant-token-1234567890".into(),
        capabilities: vec![TokenCapability::Sessions],
        mutating_requests_per_minute: 600,
        vogt_core_token_file: None,
        vogt_core_token: None,
    }];
    let (base, _h) = boot_with_config(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth_for("no-assistant-token-1234567890"))
        .build()
        .unwrap();

    let stt = client
        .post(format!("{base}/api/assistant/stt"))
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(stt.status(), StatusCode::FORBIDDEN);

    let tts = client
        .post(format!("{base}/api/assistant/tts"))
        .json(&json!({ "text": "hello" }))
        .send()
        .await
        .unwrap();
    assert_eq!(tts.status(), StatusCode::FORBIDDEN);
}

/// `/api/config` advertises each configured speech half by presence only — never
/// a key or a base URL — so a client can pick the server pipeline by capability
/// rather than by probing for a 404.
#[tokio::test]
async fn config_advertises_configured_server_speech() {
    let (base, _h) = boot().await;
    let unconfigured: Value = reqwest::get(format!("{base}/api/config"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(unconfigured["assistant_stt_enabled"], false);
    assert_eq!(unconfigured["assistant_tts_enabled"], false);

    let mut cfg = test_config();
    cfg.assistant_stt_base_urls = vec!["https://audio.invalid/v1".into()];
    cfg.assistant_stt_api_key = Some("sk-stt-123".into());
    // TTS left unset: the two halves are independent.
    let (base, _h) = boot_with_config(cfg).await;
    let configured: Value = reqwest::get(format!("{base}/api/config"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(configured["assistant_stt_enabled"], true);
    assert_eq!(configured["assistant_tts_enabled"], false);
    // Presence only — the key and base URL never appear.
    let rendered = configured.to_string();
    assert!(!rendered.contains("sk-stt-123"), "the key must never leak");
    assert!(
        !rendered.contains("audio.invalid"),
        "the base URL is an exposure value and must never leak"
    );
}

// --- #289: fail-closed approval gates and mid-run steering -----------------

/// A config whose workspace and state dir are a throwaway tempdir. The dir must
/// outlive the server, so the caller holds the returned `TempDir`.
fn agent_task_config() -> (Config, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = test_config();
    cfg.default_cwd = tmp.path().to_path_buf();
    cfg.workspace_root = tmp.path().canonicalize().unwrap();
    cfg.state_dir = tmp.path().join("state");
    (cfg, tmp)
}

/// Create a task and start one run of it, returning `(task_id, run_id)`.
async fn create_and_run(client: &reqwest::Client, base: &str, body: Value) -> (String, String) {
    let created: Value = client
        .post(format!("{base}/api/agent-tasks"))
        .json(&body)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let task_id = created["id"].as_str().unwrap().to_string();
    let run: Value = client
        .post(format!("{base}/api/agent-tasks/{task_id}/run"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let run_id = run["id"].as_str().unwrap().to_string();
    (task_id, run_id)
}

/// Poll a task's latest run until its first gate satisfies `pred`, or time out.
async fn wait_for_gate<F>(client: &reqwest::Client, base: &str, task_id: &str, pred: F) -> Value
where
    F: Fn(&Value) -> bool,
{
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            tokio::time::sleep(Duration::from_millis(40)).await;
            let detail: Value = client
                .get(format!("{base}/api/agent-tasks/{task_id}"))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            if let Some(gate) = detail["runs"][0]["gates"].get(0) {
                if !gate.is_null() && pred(gate) {
                    break gate.clone();
                }
            }
        }
    })
    .await
    .expect("the gate should reach the awaited state")
}

/// Decode a session's scrollback to a lossy string, for asserting what reached
/// the PTY.
async fn session_scrollback(client: &reqwest::Client, base: &str, session_id: &str) -> String {
    let session: SessionDetail = client
        .get(format!("{base}/api/sessions/{session_id}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(session.scrollback_base64.as_bytes())
        .unwrap();
    String::from_utf8_lossy(&bytes).into_owned()
}

fn approve_hold_gate() -> Value {
    json!({
        "question": "Proceed with the deploy?",
        "options": [
            { "label": "Approve", "input": "go", "approve": true },
            { "label": "Hold", "input": "stop", "approve": false },
        ],
    })
}

/// A declared gate opens at the run's prompt boundary and holds the PTY: the
/// session stays alive and the run stays running until the gate is answered.
#[tokio::test]
async fn a_declared_gate_opens_and_holds_the_run() {
    let (cfg, _tmp) = agent_task_config();
    let (base, _h) = boot_with_config(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    let (task_id, _run) = create_and_run(
        &client,
        &base,
        json!({
            "name": "gated idle",
            "prompt": "Wait at a gate.",
            "schedule": { "kind": "manual" },
            "command": fake_agent_command("idle"),
            "gates": [ approve_hold_gate() ],
        }),
    )
    .await;

    let gate = wait_for_gate(&client, &base, &task_id, |g| g["state"] == "open").await;
    assert_eq!(gate["question"], "Proceed with the deploy?");

    // Held: after the gate opens, the run does not proceed on its own.
    tokio::time::sleep(Duration::from_millis(400)).await;
    let detail: Value = client
        .get(format!("{base}/api/agent-tasks/{task_id}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        detail["runs"][0]["status"], "running",
        "an unanswered gate must hold the run"
    );
    assert_eq!(detail["runs"][0]["gates"][0]["state"], "open");

    // End the held run so the fake-agent exits: a session left blocked on its
    // stdin keeps a `child.wait()` blocking task alive, which the test
    // runtime's shutdown would otherwise wait on forever.
    let session_id = detail["runs"][0]["session_id"].as_str().unwrap();
    client
        .post(format!("{base}/api/sessions/{session_id}/kill"))
        .send()
        .await
        .unwrap();
}

/// Answering an open gate resolves it to the chosen option, delivers that
/// option's input to the PTY, and lets the run finish.
#[tokio::test]
async fn answering_a_gate_resolves_it_and_the_run_continues() {
    let (cfg, _tmp) = agent_task_config();
    let (base, _h) = boot_with_config(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    let (task_id, _run) = create_and_run(
        &client,
        &base,
        json!({
            "name": "gated idle",
            "prompt": "Wait at a gate.",
            "schedule": { "kind": "manual" },
            "command": fake_agent_command("idle"),
            "gates": [ approve_hold_gate() ],
        }),
    )
    .await;

    let gate = wait_for_gate(&client, &base, &task_id, |g| g["state"] == "open").await;
    let gate_id = gate["id"].as_str().unwrap().to_string();

    let answered: Value = client
        .post(format!(
            "{base}/api/agent-tasks/{task_id}/gates/{gate_id}/answer"
        ))
        .json(&json!({ "option": 0, "actor": "sprooty" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(answered["state"], "answered");
    assert_eq!(answered["actor"], "sprooty");
    assert_eq!(answered["approved"], true);

    let detail = wait_for_run_finish(&client, &base, &task_id).await;
    assert_eq!(detail["runs"][0]["status"], "completed");
    let session_id = detail["runs"][0]["session_id"].as_str().unwrap();
    let text = session_scrollback(&client, &base, session_id).await;
    assert!(
        text.contains("steered with 'go'"),
        "the approve option's input should reach the PTY: {text:?}"
    );
}

/// Fail closed on death: a session killed with a gate held resolves the gate to
/// `blocked`, never approved.
#[tokio::test]
async fn a_gate_whose_session_dies_fails_closed_to_blocked() {
    let (cfg, _tmp) = agent_task_config();
    let (base, _h) = boot_with_config(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    let (task_id, _run) = create_and_run(
        &client,
        &base,
        json!({
            "name": "gated idle",
            "prompt": "Wait at a gate.",
            "schedule": { "kind": "manual" },
            "command": fake_agent_command("idle"),
            "gates": [ approve_hold_gate() ],
        }),
    )
    .await;

    let gate = wait_for_gate(&client, &base, &task_id, |g| g["state"] == "open").await;
    assert_eq!(gate["state"], "open");
    let detail: Value = client
        .get(format!("{base}/api/agent-tasks/{task_id}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let session_id = detail["runs"][0]["session_id"]
        .as_str()
        .unwrap()
        .to_string();

    client
        .post(format!("{base}/api/sessions/{session_id}/kill"))
        .send()
        .await
        .unwrap();

    let blocked = wait_for_gate(&client, &base, &task_id, |g| g["state"] == "blocked").await;
    assert_eq!(blocked["state"], "blocked");
    assert!(
        blocked.get("approved").is_none(),
        "a blocked gate is never approved"
    );
    assert!(blocked["reason"].as_str().unwrap().contains("session"));
}

/// Fail closed on timeout: a gate nobody answers within its deadline resolves
/// to `blocked` and the run ends errored.
#[tokio::test]
async fn a_gate_that_times_out_fails_closed_to_blocked() {
    let (cfg, _tmp) = agent_task_config();
    let (base, _h) = boot_with_config(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    let mut gate = approve_hold_gate();
    gate["timeout_ms"] = json!(300);
    let (task_id, _run) = create_and_run(
        &client,
        &base,
        json!({
            "name": "gated idle",
            "prompt": "Wait at a gate.",
            "schedule": { "kind": "manual" },
            "command": fake_agent_command("idle"),
            "gates": [ gate ],
        }),
    )
    .await;

    let blocked = wait_for_gate(&client, &base, &task_id, |g| g["state"] == "blocked").await;
    assert!(
        blocked["reason"].as_str().unwrap().contains("timed out"),
        "the block reason should name the timeout: {blocked:?}"
    );

    let detail = wait_for_run_finish(&client, &base, &task_id).await;
    assert_eq!(
        detail["runs"][0]["status"], "errored",
        "a run whose gate blocked cannot complete successfully"
    );
}

/// `--auto-approve` is the one bypass: it answers the gate with its approve
/// option without a human, and the resolution is audited as `auto-approve`.
#[tokio::test]
async fn auto_approve_bypasses_the_gate_and_is_audited() {
    let (cfg, _tmp) = agent_task_config();
    let (base, _h) = boot_with_config(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    let (task_id, _run) = create_and_run(
        &client,
        &base,
        json!({
            "name": "auto gated idle",
            "prompt": "Wait at a gate, but approve it yourself.",
            "schedule": { "kind": "manual" },
            "command": fake_agent_command("idle"),
            "gates": [ approve_hold_gate() ],
            "auto_approve": true,
        }),
    )
    .await;

    let answered = wait_for_gate(&client, &base, &task_id, |g| g["state"] == "answered").await;
    assert_eq!(answered["actor"], "auto-approve");
    assert_eq!(answered["auto"], true);
    assert_eq!(answered["approved"], true);

    let detail = wait_for_run_finish(&client, &base, &task_id).await;
    assert_eq!(detail["runs"][0]["status"], "completed");
}

/// A queued steer is delivered to the PTY at the next prompt boundary.
#[tokio::test]
async fn a_queued_steer_reaches_the_pty_at_the_next_boundary() {
    let (cfg, _tmp) = agent_task_config();
    let (base, _h) = boot_with_config(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    let (task_id, _run) = create_and_run(
        &client,
        &base,
        json!({
            "name": "steerable idle",
            "prompt": "Idle until steered.",
            "schedule": { "kind": "manual" },
            "command": fake_agent_command("idle"),
        }),
    )
    .await;

    let steered: Value = client
        .post(format!("{base}/api/agent-tasks/{task_id}/steer"))
        .json(&json!({ "text": "focus here", "actor": "sprooty" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(steered["ok"], true);

    let detail = wait_for_run_finish(&client, &base, &task_id).await;
    assert_eq!(detail["runs"][0]["status"], "completed");
    let session_id = detail["runs"][0]["session_id"].as_str().unwrap();
    let text = session_scrollback(&client, &base, session_id).await;
    assert!(
        text.contains("steered with 'focus here'"),
        "the steer text should reach the PTY at the boundary: {text:?}"
    );
}

/// `interrupt=true` sends the CLI's cancel (Ctrl-C) first: it reaches the idle
/// fake-agent as a SIGINT, ending the run — proof the cancel was delivered
/// ahead of the text.
#[tokio::test]
async fn an_interrupting_steer_cancels_the_cli_first() {
    let (cfg, _tmp) = agent_task_config();
    let (base, _h) = boot_with_config(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    let (task_id, _run) = create_and_run(
        &client,
        &base,
        json!({
            "name": "interruptible idle",
            "prompt": "Idle until interrupted.",
            "schedule": { "kind": "manual" },
            "command": fake_agent_command("idle"),
        }),
    )
    .await;

    // Give the run a moment to reach its idle boundary before interrupting.
    tokio::time::sleep(Duration::from_millis(300)).await;
    client
        .post(format!("{base}/api/agent-tasks/{task_id}/steer"))
        .json(&json!({ "text": "stop", "interrupt": true, "actor": "sprooty" }))
        .send()
        .await
        .unwrap();

    let detail = wait_for_run_finish(&client, &base, &task_id).await;
    assert_eq!(
        detail["runs"][0]["status"], "errored",
        "the cancel should interrupt the idle CLI, ending the run: {detail:?}"
    );
}

/// Steering a task with no run in flight is a 409, not a silent no-op: there is
/// nothing to deliver the text to.
#[tokio::test]
async fn steering_a_task_with_no_live_run_is_refused() {
    let (cfg, _tmp) = agent_task_config();
    let (base, _h) = boot_with_config(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    let created: Value = client
        .post(format!("{base}/api/agent-tasks"))
        .json(&json!({
            "name": "never run",
            "prompt": "Nothing runs.",
            "schedule": { "kind": "manual" },
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let task_id = created["id"].as_str().unwrap();

    let res = client
        .post(format!("{base}/api/agent-tasks/{task_id}/steer"))
        .json(&json!({ "text": "hello" }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CONFLICT);
}

// --- #291: typed outcomes, the conclusion record, schema-validated findings -

/// A committable git repo under `dir`, so a run's workspace has a branch and a
/// base sha for the conclusion to report against.
fn init_git_repo(dir: &std::path::Path) {
    std::fs::create_dir_all(dir).unwrap();
    for args in [
        vec!["init", "-q"],
        vec!["config", "user.email", "seed@vogt.invalid"],
        vec!["config", "user.name", "seed"],
    ] {
        let status = std::process::Command::new("git")
            .args(&args)
            .current_dir(dir)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?} failed");
    }
}

/// A clean exit in a git workspace concludes `succeeded`, and the conclusion
/// carries the run's duration, exit code, the final sha of the bound branch and
/// the diff stats for what the run left on it (#291).
#[tokio::test]
async fn a_succeeded_run_records_a_conclusion_with_git_stats() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    init_git_repo(&repo);

    let mut cfg = test_config();
    cfg.default_cwd = tmp.path().to_path_buf();
    cfg.workspace_root = tmp.path().canonicalize().unwrap();
    cfg.state_dir = tmp.path().join("state");
    let (base, _h) = boot_with_config(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    let (task_id, _run) = create_and_run(
        &client,
        &base,
        json!({
            "name": "succeeds",
            "prompt": "Make a checkpoint.",
            "schedule": { "kind": "manual" },
            "command": fake_agent_command("edit+commit"),
            "cwd": repo.to_string_lossy(),
        }),
    )
    .await;

    let detail = wait_for_run_finish(&client, &base, &task_id).await;
    let run = &detail["runs"][0];
    assert_eq!(run["status"], "completed");
    assert_eq!(run["outcome"], "succeeded");

    let concl = &run["conclusion"];
    assert_eq!(concl["outcome"], "succeeded");
    assert_eq!(concl["exit_code"], 0);
    assert!(
        concl["duration_ms"].as_u64().unwrap() > 0,
        "a run that ran took some time: {concl}"
    );
    assert!(
        concl["final_sha"].as_str().is_some_and(|s| s.len() >= 7),
        "the bound branch has a tip sha: {concl}"
    );
    let diff = &concl["diffstat"];
    assert!(
        diff["files"].as_u64().unwrap() >= 1,
        "edit+commit touched at least one file: {concl}"
    );
    assert!(
        diff["insertions"].as_u64().unwrap() >= 1,
        "edit+commit added at least one line: {concl}"
    );
}

/// A non-zero exit concludes `failed`, and the conclusion records the code.
#[tokio::test]
async fn a_nonzero_exit_concludes_failed() {
    let (cfg, _tmp) = agent_task_config();
    let (base, _h) = boot_with_config(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    let (task_id, _run) = create_and_run(
        &client,
        &base,
        json!({
            "name": "fails",
            "prompt": "Exit non-zero.",
            "schedule": { "kind": "manual" },
            "command": fake_agent_command("outcome"),
            "env": [["FAKE_AGENT_EXIT_CODE", "7"]],
        }),
    )
    .await;

    let detail = wait_for_run_finish(&client, &base, &task_id).await;
    let run = &detail["runs"][0];
    assert_eq!(run["status"], "errored");
    assert_eq!(run["outcome"], "failed");
    assert_eq!(run["conclusion"]["exit_code"], 7);
}

/// A run that prints the skip sentinel and exits cleanly concludes `skipped` —
/// distinct from `succeeded`, so a reader can tell "nothing to do" from "did it".
#[tokio::test]
async fn a_skip_sentinel_concludes_skipped() {
    let (cfg, _tmp) = agent_task_config();
    let (base, _h) = boot_with_config(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    let (task_id, _run) = create_and_run(
        &client,
        &base,
        json!({
            "name": "skips",
            "prompt": "Nothing to do.",
            "schedule": { "kind": "manual" },
            "command": fake_agent_command("skip"),
            "env": [["FAKE_AGENT_SKIP_REASON", "already current"]],
        }),
    )
    .await;

    let detail = wait_for_run_finish(&client, &base, &task_id).await;
    let run = &detail["runs"][0];
    assert_eq!(run["outcome"], "skipped");
    assert_eq!(run["conclusion"]["outcome"], "skipped");
}

/// A gate that fails closed (here by timing out) concludes the run `blocked` —
/// the #289 fail-closed path surfaced as the #291 verdict.
#[tokio::test]
async fn a_gate_that_times_out_concludes_blocked() {
    let (cfg, _tmp) = agent_task_config();
    let (base, _h) = boot_with_config(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    let (task_id, _run) = create_and_run(
        &client,
        &base,
        json!({
            "name": "gated",
            "prompt": "Wait at a gate that no one answers.",
            "schedule": { "kind": "manual" },
            "command": fake_agent_command("idle"),
            "gates": [ {
                "question": "Proceed?",
                "options": [ { "label": "Approve", "input": "go", "approve": true } ],
                "timeout_ms": 300,
            } ],
        }),
    )
    .await;

    // The gate fails closed on its deadline, which kills the session.
    let gate = wait_for_gate(&client, &base, &task_id, |g| g["state"] == "blocked").await;
    assert_eq!(gate["state"], "blocked");

    let detail = wait_for_run_finish(&client, &base, &task_id).await;
    let run = &detail["runs"][0];
    assert_eq!(run["outcome"], "blocked");
    assert_eq!(run["conclusion"]["outcome"], "blocked");
}

fn findings_schema() -> Value {
    json!({
        "type": "object",
        "required": ["summary", "risk"],
        "properties": {
            "summary": { "type": "string" },
            "risk": { "type": "string", "enum": ["low", "medium", "high"] },
        },
    })
}

/// Findings that match the `output_schema` first try conclude `succeeded` with
/// no re-prompts.
#[tokio::test]
async fn output_schema_that_matches_first_try_concludes_succeeded() {
    let (cfg, _tmp) = agent_task_config();
    let (base, _h) = boot_with_config(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    let (task_id, _run) = create_and_run(
        &client,
        &base,
        json!({
            "name": "schema-pass",
            "prompt": "Report structured findings.",
            "schedule": { "kind": "manual" },
            "command": fake_agent_command("schema"),
            "output_schema": findings_schema(),
            "env": [["FAKE_AGENT_SCHEMA_PASS_ON", "1"]],
        }),
    )
    .await;

    let detail = wait_for_run_finish(&client, &base, &task_id).await;
    let run = &detail["runs"][0];
    assert_eq!(run["outcome"], "succeeded", "run: {run}");
    assert_eq!(run["retries"], 0);
    assert_eq!(run["schema_ok"], true);
}

/// Findings that never match are re-prompted up to the budget, then the run
/// concludes `partially-succeeded` with the re-prompt count recorded.
#[tokio::test]
async fn output_schema_that_never_matches_concludes_partially_succeeded_after_retries() {
    let (cfg, _tmp) = agent_task_config();
    let (base, _h) = boot_with_config(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    let (task_id, _run) = create_and_run(
        &client,
        &base,
        json!({
            "name": "schema-fail",
            "prompt": "Report structured findings it cannot get right.",
            "schedule": { "kind": "manual" },
            "command": fake_agent_command("schema"),
            "output_schema": findings_schema(),
            "output_schema_max_retries": 2,
            // Never reaches a good block, so every attempt fails validation.
            "env": [["FAKE_AGENT_SCHEMA_PASS_ON", "9"]],
        }),
    )
    .await;

    let detail = wait_for_run_finish(&client, &base, &task_id).await;
    let run = &detail["runs"][0];
    assert_eq!(run["outcome"], "partially-succeeded", "run: {run}");
    assert_eq!(run["retries"], 2, "two re-prompts were spent: {run}");
    assert_eq!(run["schema_ok"], false);
    assert_eq!(run["conclusion"]["outcome"], "partially-succeeded");
}

/// A `VOGT_COST:` line the run prints is parsed into the conclusion's cost.
#[tokio::test]
async fn a_cost_line_is_parsed_into_the_conclusion() {
    let (cfg, _tmp) = agent_task_config();
    let (base, _h) = boot_with_config(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    let (task_id, _run) = create_and_run(
        &client,
        &base,
        json!({
            "name": "reports-cost",
            "prompt": "Report a cost then finish.",
            "schedule": { "kind": "manual" },
            "command": fake_agent_command("cost"),
            "env": [["FAKE_AGENT_COST", "{\"total_usd\": 0.42, \"input_tokens\": 1200}"]],
        }),
    )
    .await;

    let detail = wait_for_run_finish(&client, &base, &task_id).await;
    let run = &detail["runs"][0];
    assert_eq!(run["outcome"], "succeeded");
    let cost = &run["conclusion"]["cost"];
    assert_eq!(cost["total_usd"].as_f64().unwrap(), 0.42);
    assert_eq!(cost["input_tokens"].as_u64().unwrap(), 1200);
}
