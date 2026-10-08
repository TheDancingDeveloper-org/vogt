//! End-to-end integration tests: start the real Axum server on an OS-assigned
//! port, talk to it over HTTP + WebSocket the same way a client would.

use flate2::read::GzDecoder;
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
        token: Some(TEST_TOKEN.to_string()),
        token_mutating_request_limit_per_minute: 600,
        scrollback_bytes: 64 * 1024,
        default_shell: "/bin/bash".to_string(),
        default_cwd: std::env::temp_dir(),
        activity_idle_after_ms: 200,
        idle_stall_after_ms: 10 * 60 * 1_000,
        workspace_root: std::env::temp_dir(),
        gui_stream_url: None,
        gui_stream_verified: false,
        ws_query_token_allowed: false,
        // The push tests subscribe a loopback http stand-in service.
        push_allow_insecure_endpoints: true,
        state_dir: tempfile::tempdir().unwrap().keep(),
        fcm_service_account_json: None,
        vapid_subject: "mailto:test@example.invalid".to_string(),
        allowed_origins: vec![],
        auto_agent_auth: false,
        agent_auth_helper: "/usr/local/bin/vogt-agent-auth".into(),
        agent_auth_secrets: vec![],
        agent_grant_projects: vec![],
        // The synthetic agent CLI is registered as a session preset in the
        // *test* config — never the production defaults — so agent-task
        // scenarios can be driven without a real `claude`/`codex` in a PTY
        // A run selects it by taking the preset's command, exactly as
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
        history_retention_days: 30,
        history_live_scan_bytes: 256 * 1024,
        assistant_stt_base_urls: vec![],
        assistant_stt_api_key: None,
        assistant_stt_model: "whisper-1".into(),
        assistant_stt_language: "en".into(),
        assistant_tts_base_urls: vec![],
        assistant_tts_api_key: None,
        assistant_tts_model: "tts-1".into(),
        assistant_tts_voice: "alloy".into(),
        assistant_tts_format: "mp3".into(),
        assistant_speech_attempt_timeout_ms: 30_000,
        public_url: None,
        vogt_core_url: None,
        vogt_import_root: None,
        vogt_engine_state_dir: None,
        vogt_core_token: None,
        agent_clis: vogt_engine_server::agent_clis::AgentCliPaths::default(),
        hibernation: vogt_engine_server::hibernate_policy::Policy::default(),
        assistant_call: vogt_engine_server::call::CallPolicy::default(),
        autopilot: vogt_engine_server::autopilot::Policy::default(),
        agent_onboarding: vogt_engine_server::claude_config::Onboarding::default(),
        session_rss_warn_bytes: None,
        metrics_bind: None,
    }
}

async fn boot() -> (String, ServerGuard) {
    boot_with_config(test_config()).await
}

/// Boot with a chosen scrollback ring size, leaving every other test-config
/// default in place. The suite default stays 64 KiB (`test_config`); the
/// large-session resume tests use this to size the ring around the flood they
/// are exercising (a tiny ring an early cursor ages out of, or the 4 MiB ring
/// prod runs). A per-session `scrollback_bytes` override on session-create also
/// works, but sizing the whole engine keeps each test's intent in one place.
#[allow(dead_code)] // used by the large-session resume tests
async fn boot_with(scrollback_bytes: usize) -> (String, ServerGuard) {
    boot_with_config(Config {
        scrollback_bytes,
        ..test_config()
    })
    .await
}

async fn boot_with_config(cfg: Config) -> (String, ServerGuard) {
    let (base, _state, handle) = boot_with_state(cfg).await;
    (base, handle)
}

/// Boot and keep the `AppState`, so a test can publish onto the engine's own
/// event bus — the same bus `vogt_core::spawn_event_follower` republishes
/// vogt-core's events onto — and drive the agent-task trigger watcher
/// with synthetic core events, without needing a real vogt-core beside it.
async fn boot_with_state(cfg: Config) -> (String, Arc<AppState>, ServerGuard) {
    let (router, state) = router(cfg).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    let guard = ServerGuard {
        server: handle,
        state: Arc::clone(&state),
    };
    (format!("http://{addr}"), state, guard)
}

/// Owns a booted test engine and reaps it when the test ends, however it ends.
///
/// Every session child gets a `spawn_blocking` exit waiter blocked in
/// `child.wait()`, and dropping a `#[tokio::test]` runtime waits for its
/// blocking pool with no timeout. A test that panics before its trailing
/// `kill_session` therefore used to hang until the child exited on its own:
/// an hour for the load sessions (`exec sleep 3600`), forever for `/bin/cat`.
/// On a slow CI runner that turned a seconds-long assertion failure into a
/// 45-minute wedged job with no test name in the log. Dropping this guard
/// SIGKILLs every session the engine still holds, so a failing test fails in
/// seconds with its own panic message.
struct ServerGuard {
    server: tokio::task::JoinHandle<()>,
    state: Arc<AppState>,
}

impl Drop for ServerGuard {
    fn drop(&mut self) {
        for session in self.state.sessions.live_sessions() {
            let _ = session.kill();
        }
        self.server.abort();
    }
}

/// A synthetic vogt-core event on the engine bus, the shape the follower
/// publishes — used to drive the trigger watcher in tests.
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

/// A stand-in vogt-core that answers `GET /api/auth/whoami` for the bearers
/// it is told about and 401 for everything else. The front door holds no
/// token table of its own, so a test that needs a caller with *less* than
/// full capability gives the core an identity to resolve.
async fn stand_in_core_knowing(
    identities: Vec<(&'static str, &'static str, Vec<&'static str>)>,
) -> String {
    use axum::{extract::State, http::HeaderMap, response::IntoResponse, routing::any, Router};
    type Known = Arc<Vec<(&'static str, &'static str, Vec<&'static str>)>>;
    async fn handler(
        State(known): State<Known>,
        headers: HeaderMap,
        uri: axum::http::Uri,
    ) -> axum::response::Response {
        if uri.path() == "/api/auth/whoami" {
            let bearer = headers
                .get(axum::http::header::AUTHORIZATION)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.strip_prefix("Bearer "))
                .unwrap_or_default();
            return match known.iter().find(|(token, _, _)| *token == bearer) {
                Some((_, identity_ref, scopes)) => (
                    StatusCode::OK,
                    axum::Json(json!({
                        "identity_ref": identity_ref,
                        "kind": "human",
                        "display_name": identity_ref,
                        "scopes": scopes,
                    })),
                )
                    .into_response(),
                None => {
                    (StatusCode::UNAUTHORIZED, axum::Json(json!({"error": "no"}))).into_response()
                }
            };
        }
        (StatusCode::OK, axum::Json(json!({"seen": uri.path()}))).into_response()
    }
    let app = Router::new()
        .route("/{*path}", any(handler))
        .with_state(Arc::new(identities));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{addr}")
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
/// Resolved from the crate manifest so the test does not care what the
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
        description: "Deterministic synthetic agent CLI for agent-task tests".to_string(),
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
    assert!(
        body["product_version"].as_str().is_some(),
        "missing product version"
    );
    assert!(body["source_ref"].as_str().is_some(), "missing source ref");
    assert!(body["source_sha"].as_str().is_some(), "missing source sha");
}

#[tokio::test]
async fn finite_pwa_and_api_responses_negotiate_gzip_without_changing_cache_policy() {
    let (base, _h) = boot().await;
    let client = reqwest::Client::builder()
        .no_gzip()
        .no_brotli()
        .build()
        .unwrap();

    let identity = client
        .get(format!("{base}/"))
        .header(reqwest::header::ACCEPT_ENCODING, "identity")
        .send()
        .await
        .unwrap();
    let identity_body = identity.bytes().await.unwrap();

    let compressed = client
        .get(format!("{base}/"))
        .header(reqwest::header::ACCEPT_ENCODING, "gzip")
        .send()
        .await
        .unwrap();
    assert_eq!(
        compressed.headers()[reqwest::header::CONTENT_ENCODING],
        "gzip"
    );
    assert_eq!(
        compressed.headers()[reqwest::header::VARY]
            .to_str()
            .unwrap()
            .to_ascii_lowercase(),
        "accept-encoding"
    );
    assert_eq!(
        compressed.headers()[reqwest::header::CACHE_CONTROL],
        "no-store, must-revalidate"
    );
    let compressed_body = compressed.bytes().await.unwrap();
    let mut decoder = GzDecoder::new(&compressed_body[..]);
    let mut decoded = Vec::new();
    std::io::Read::read_to_end(&mut decoder, &mut decoded).unwrap();
    assert_eq!(decoded, identity_body);

    let api = client
        .get(format!("{base}/api/config"))
        .header(reqwest::header::ACCEPT_ENCODING, "gzip")
        .send()
        .await
        .unwrap();
    assert_eq!(api.headers()[reqwest::header::CONTENT_ENCODING], "gzip");
    assert_eq!(
        api.headers()[reqwest::header::VARY]
            .to_str()
            .unwrap()
            .to_ascii_lowercase(),
        "accept-encoding"
    );
    let api_body = api.bytes().await.unwrap();
    let mut decoder = GzDecoder::new(&api_body[..]);
    let mut decoded = Vec::new();
    std::io::Read::read_to_end(&mut decoder, &mut decoded).unwrap();
    serde_json::from_slice::<Value>(&decoded).unwrap();
}

#[tokio::test]
async fn compression_does_not_modify_sse_or_identity_responses() {
    let (base, _h) = boot().await;
    let client = reqwest::Client::builder()
        .no_gzip()
        .no_brotli()
        .build()
        .unwrap();

    let sse = client
        .get(format!("{base}/api/events"))
        .header(reqwest::header::ACCEPT_ENCODING, "gzip")
        .header(
            reqwest::header::AUTHORIZATION,
            format!("Bearer {TEST_TOKEN}"),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(sse.status(), StatusCode::OK);
    assert!(!sse
        .headers()
        .contains_key(reqwest::header::CONTENT_ENCODING));
    assert_eq!(
        sse.headers()[reqwest::header::CONTENT_TYPE],
        "text/event-stream"
    );

    let identity = client
        .get(format!("{base}/api/config"))
        .header(reqwest::header::ACCEPT_ENCODING, "identity")
        .send()
        .await
        .unwrap();
    assert!(!identity
        .headers()
        .contains_key(reqwest::header::CONTENT_ENCODING));
}

#[tokio::test]
async fn auth_check_is_cheap_and_requires_auth() {
    let (base, _h) = boot().await;

    let unauth = reqwest::get(format!("{base}/api/auth/check"))
        .await
        .unwrap();
    assert_eq!(unauth.status(), StatusCode::UNAUTHORIZED);

    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let body: Value = client
        .get(format!("{base}/api/auth/check"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(body["ok"], true);
    assert!(body["version"].as_str().is_some(), "missing version");
    assert!(body["storage"]["workspace_root"].as_str().is_some());
    assert!(body.get("history").is_none());
    assert!(body.get("agent_tasks").is_none());
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
        body["product_version"].as_str().is_some(),
        "missing product version"
    );
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
            "command": ["/bin/sh", "-lc", "printf 'task:%s run:%s\\n' \"$VOGT_ENGINE_AGENT_TASK_ID\" \"$VOGT_ENGINE_AGENT_TASK_RUN_ID\""],
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let task_id = created["id"].as_str().unwrap().to_string();
    assert_eq!(created["name"], "PX3 price monitor");
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

/// A bound task's subject and its recorded findings, both halves in one run.
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
                "sleep 0.3; printf 'VOGT_NOTIFY:bound to %s in %s\\n' \"$VOGT_WORK_ITEM\" \"$VOGT_PROJECT\""],
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

/// The fake-agent is registered as a session preset, so a client can
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

/// Fake-agent edit+commit, driven all the way through the engine's agent-task path.
///
/// The run's working tree is a real git repo; the fake-agent edits a file and
/// commits it with the checkpoint trailers, and the trailer carrying the run
/// id must equal the run the engine actually started. This is the seam the
/// future git story needs: a checkpoint a run made, traceable back
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

    // The engine set VOGT_ENGINE_AGENT_TASK_RUN_ID for the run; the fake-agent
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

/// Fake-agent findings, driven through the engine's phrase watcher.
///
/// The fake-agent prints a `VOGT_NOTIFY:` line; the engine records the text
/// after it as a durable finding on the run. No push service, no real
/// agent — just the parse-and-record path typed outcomes build on.
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

/// Fake-agent outcome: a chosen non-zero exit is surfaced as an errored run.
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

/// A `work.transitioned` core event starts a bound, audited run.
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

/// Drift, observation, and forge-PR-check events each start the right
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

/// The per-task concurrency cap holds, and a fire it cannot honour is
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

/// An `api` fire is refused unless the task has an `api` trigger armed,
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
    // which session it is: `VOGT_ENGINE_SESSION` is only the display name, which
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
            "command": ["/bin/sh", "-lc", "printf %s \"$VOGT_ENGINE_SESSION_ID\""],
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

/// Run a one-shot command in a new session and return what it printed once
/// it holds `expected`.
async fn session_output(base: &str, body: Value, expected: &str) -> String {
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let created: Value = client
        .post(format!("{base}/api/sessions"))
        .json(&body)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = created["id"].as_str().unwrap().to_string();
    session_output_after_exit(&client, base, &id, &[expected]).await
}

#[tokio::test]
async fn every_session_is_told_the_engine_url_and_a_caller_can_override_it() {
    // A session opened from the GUI never passes through vogt-core, which is
    // what used to set VOGT_ENGINE_URL; the engine now sets it itself, from
    // its bind address (a wildcard bind reads as loopback).
    let (base, _h) = boot_with_config(Config {
        bind: "0.0.0.0:48910".parse().unwrap(),
        ..test_config()
    })
    .await;
    let printed = session_output(
        &base,
        json!({
            "name": "engine-url-env",
            "command": ["/bin/sh", "-lc", "printf '<%s>' \"$VOGT_ENGINE_URL\""],
        }),
        "<http://127.0.0.1:48910>",
    )
    .await;
    assert!(
        printed.contains("<http://127.0.0.1:48910>"),
        "child should see the engine URL; got {printed:?}"
    );

    // The caller's value (vogt-core's view of the engine) wins.
    let printed = session_output(
        &base,
        json!({
            "name": "engine-url-env-override",
            "command": ["/bin/sh", "-lc", "printf '<%s>' \"$VOGT_ENGINE_URL\""],
            "env": [["VOGT_ENGINE_URL", "http://engine.example:8910"]],
        }),
        "<http://engine.example:8910>",
    )
    .await;
    assert!(
        printed.contains("<http://engine.example:8910>"),
        "a caller-supplied VOGT_ENGINE_URL should win; got {printed:?}"
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
                "printf '<img src=x onerror=globalThis.__vogt_xss=1> <svg onload=1> <broken history-needle path/with &lt;script&gt;alert(1)&lt;/script&gt; punctuation\\n'; exit 7",
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

    // Wait for the *finalized* row, not merely the provisional one written at
    // spawn. The provisional row appears first with a NULL exit code;
    // the exit waiter later upserts the real outcome. Requiring exit_code == 7
    // in the predicate settles that race deterministically.
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
        if let Some(session) = sessions
            .iter()
            .find(|s| s["id"] == id && s["exit_code"] == json!(7))
        {
            break session.clone();
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "session was not archived with its final exit code; got {sessions:?}"
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
    // on an idle machine and loses under load — a CI pipeline failed exactly
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
            let hit = hits
                .iter()
                .find(|hit| hit["session_id"] == id)
                .expect("matching history result");
            let snippet = hit["match_snippet"].as_str().unwrap_or_default();
            assert!(
                !snippet.contains("<mark>"),
                "history API must return plain text snippets: {hit:?}"
            );
            assert!(
                snippet.contains("<img src=x onerror=globalThis.__vogt_xss=1>"),
                "history API must preserve hostile output as data: {hit:?}"
            );
            assert!(
                snippet.contains("<svg onload=1>"),
                "history API must preserve SVG payloads as data: {hit:?}"
            );
            assert!(
                snippet.contains("<broken"),
                "history API must preserve malformed markup as data: {hit:?}"
            );
            assert!(
                snippet.contains("&lt;script&gt;alert(1)&lt;/script&gt;"),
                "history API must preserve encoded payloads as data: {hit:?}"
            );
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

/// A long-lived session that never runs `exit` is still archived when the
/// engine shuts down, via the graceful-shutdown drain — it appears in history
/// with `ended_at` set and no one had to type `exit`.
#[tokio::test]
async fn shutdown_drain_archives_a_live_session_that_never_exited() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = test_config();
    cfg.default_cwd = tmp.path().to_path_buf();
    cfg.workspace_root = tmp.path().canonicalize().unwrap();

    let (base, state, _h) = boot_with_state(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    // `exec cat` never exits on its own: the child blocks forever on stdin,
    // exactly like a long-lived agent shell on prod.
    let id: String = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({
            "name": "never-exits",
            "command": ["/bin/sh", "-lc", "printf 'drain-me\\n'; exec cat"],
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

    // Give the reader thread a moment to flush the banner into the raw log so
    // the drain has output to archive.
    tokio::time::sleep(Duration::from_millis(200)).await;

    // The graceful-shutdown drain, invoked directly (the drain is what the
    // SIGTERM handler in `serve_forever` calls). The child is still running.
    state.sessions.drain_to_history().await;

    let sessions: Vec<Value> = client
        .get(format!("{base}/api/history/sessions?limit=20"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let archived = sessions
        .iter()
        .find(|s| s["id"] == id)
        .unwrap_or_else(|| panic!("live session was not archived by drain; got {sessions:?}"));
    assert_eq!(archived["name"], "never-exits");
    assert!(
        archived["ended_at"].is_string(),
        "drained session must have ended_at set: {archived:?}"
    );
    // The child never exited, so its outcome is unknown: exit_code stays NULL,
    // which is what the `unfinished` history filter selects.
    assert!(
        archived["exit_code"].is_null(),
        "a drained-but-unexited session has an unknown exit code: {archived:?}"
    );

    // Reap the never-exiting child so the exit waiter's blocking `child.wait()`
    // returns; otherwise it pins the test runtime open past the end of the test.
    let _ = client
        .post(format!("{base}/api/sessions/{id}/kill"))
        .send()
        .await;
}

/// A provisional row exists at spawn with a NULL exit code, and a later
/// provisional (re)write never clobbers the real outcome once the session has
/// finalized on exit.
#[tokio::test]
async fn provisional_history_row_is_written_at_spawn_and_not_clobbered() {
    use vogt_engine_server::history::ArchiveRecord;

    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = test_config();
    cfg.default_cwd = tmp.path().to_path_buf();
    cfg.workspace_root = tmp.path().canonicalize().unwrap();

    let (base, state, _h) = boot_with_state(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    // Stays alive for ~2s, giving a wide window to observe the provisional row
    // before the exit waiter finalizes it with exit code 5.
    let id: String = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({
            "name": "provisional",
            "command": ["/bin/sh", "-lc", "printf 'ready\\n'; sleep 2; exit 5"],
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

    // The provisional row is written (asynchronously) at spawn: poll for it and
    // assert it is present with no exit code and no end time.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    let provisional = loop {
        let resp = client
            .get(format!("{base}/api/history/{id}"))
            .send()
            .await
            .unwrap();
        if resp.status() == StatusCode::OK {
            let row: Value = resp.json().await.unwrap();
            if row["exit_code"].is_null() {
                break row;
            }
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "provisional history row never appeared before finalize"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert!(
        provisional["ended_at"].is_null(),
        "a provisional row has no end time yet: {provisional:?}"
    );

    // After the child exits, the exit waiter upserts the real outcome over the
    // provisional NULLs.
    let finalize_deadline = tokio::time::Instant::now() + Duration::from_secs(6);
    loop {
        let row: Value = client
            .get(format!("{base}/api/history/{id}"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        if row["exit_code"] == json!(5) {
            assert!(
                row["ended_at"].is_string(),
                "finalized row must have ended_at: {row:?}"
            );
            break;
        }
        assert!(
            tokio::time::Instant::now() < finalize_deadline,
            "session never finalized with its real exit code"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // A late provisional (re)write — the race the COALESCE guard defends
    // against — must not NULL the finalized outcome.
    let history = state.history.as_ref().expect("history enabled");
    history
        .archive_session(ArchiveRecord {
            id: uuid::Uuid::parse_str(&id).unwrap(),
            name: "provisional".to_string(),
            created_at: OffsetDateTime::now_utc(),
            ended_at: None,
            exit_code: None,
            cwd: None,
            command: None,
            scrollback_bytes: 0,
            end_reason: None,
            identity: None,
        })
        .await
        .unwrap();

    let after: Value = client
        .get(format!("{base}/api/history/{id}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        after["exit_code"],
        json!(5),
        "a late provisional write must not clobber a completed exit code: {after:?}"
    );
    assert!(
        after["ended_at"].is_string(),
        "a late provisional write must not clobber ended_at: {after:?}"
    );
}

/// A raw session log that predates any index row (survived a hard
/// restart) is recovered on startup with a NULL exit code and made searchable,
/// and an already-indexed session is never duplicated.
#[tokio::test]
async fn startup_backfill_indexes_orphaned_logs_without_duplicating() {
    use vogt_engine_server::history::SessionHistory;

    let dir = tempfile::tempdir().unwrap();
    let history = SessionHistory::new(dir.path()).await.unwrap();

    // An orphaned raw log with no matching row, as a prod redeploy would leave.
    let orphan = uuid::Uuid::new_v4();
    std::fs::write(
        history.log_dir().join(format!("{orphan}.log")),
        b"orphan-backfill-needle in the transcript\n",
    )
    .unwrap();

    let recovered = history.backfill_orphaned_logs().await.unwrap();
    assert_eq!(recovered, 1, "the single orphaned log should be recovered");

    let row = history.get_session(orphan).await.unwrap();
    assert!(
        row.exit_code.is_none(),
        "a recovered log has an unknown outcome (NULL exit code)"
    );
    assert!(
        row.scrollback_bytes > 0,
        "the recovered row records the log size"
    );

    // The recovered transcript is searchable.
    let hits = history.search("orphan-backfill-needle", 10).await.unwrap();
    assert!(
        hits.iter().any(|h| h.session_id == orphan.to_string()),
        "recovered output should be searchable; got {hits:?}"
    );

    // Running again is idempotent: nothing new, and no duplicate row.
    let again = history.backfill_orphaned_logs().await.unwrap();
    assert_eq!(again, 0, "a second backfill recovers nothing");
    assert_eq!(history.count_sessions().await.unwrap(), 1);
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

    // Wait for the finalized row, not the provisional one written at spawn
    // Only after the exit waiter finalizes (non-NULL exit_code) is the
    // reader thread guaranteed to have flushed the full transcript to the raw
    // log, so the tail preview below can rely on "third line" being present.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let session = client
            .get(format!("{base}/api/history/{id}"))
            .send()
            .await
            .unwrap();
        if session.status() == StatusCode::OK {
            let row: Value = session.json().await.unwrap();
            if !row["exit_code"].is_null() {
                break;
            }
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

    // Wait for the finalized row, not the provisional one written at spawn
    // If cleanup deleted the provisional row before the exit waiter
    // finalized, the finalize's upsert would re-insert it and the 404 assert
    // below would race. Requiring a non-NULL exit_code means no further write
    // is pending when cleanup runs.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let session = client
            .get(format!("{base}/api/history/{id}"))
            .send()
            .await
            .unwrap();
        if session.status() == StatusCode::OK {
            let row: Value = session.json().await.unwrap();
            if !row["exit_code"].is_null() {
                break;
            }
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

/// A *live* session's output is searchable before it exits, via the
/// on-demand bounded scan (`include_live`, on by default), and the hit is
/// marked `live: true`. `include_live=false` restores archive-only behaviour,
/// so the same still-running session is not found there.
#[tokio::test]
async fn live_session_output_is_searchable_before_exit() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = test_config();
    cfg.default_cwd = tmp.path().to_path_buf();
    cfg.workspace_root = tmp.path().canonicalize().unwrap();

    let (base, _h) = boot_with_config(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    // Prints a unique needle then stays alive — never archived during the test.
    let id: String = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({
            "name": "live-search-me",
            "command": ["/bin/sh", "-lc", "printf 'live-needle-zzz here\\n'; sleep 30"],
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

    // The needle reaches scrollback shortly after spawn; the live scan then
    // finds it. Same deadline discipline as the archive-search test.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let hits: Vec<Value> = client
            .get(format!("{base}/api/history/search?q=live-needle-zzz"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        if let Some(hit) = hits.iter().find(|hit| hit["session_id"] == id) {
            assert_eq!(
                hit["live"], true,
                "a live-session hit must be flagged: {hit:?}"
            );
            assert!(
                hit["match_snippet"]
                    .as_str()
                    .unwrap_or_default()
                    .contains("live-needle-zzz"),
                "live snippet must preserve the matched output as data: {hit:?}"
            );
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "live search should find a running session's output; got {hits:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // Archive-only search excludes the still-running session.
    let archive_only: Vec<Value> = client
        .get(format!(
            "{base}/api/history/search?q=live-needle-zzz&include_live=false"
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        !archive_only.iter().any(|hit| hit["session_id"] == id),
        "include_live=false must not return a live (unarchived) session: {archive_only:?}"
    );
}

/// The log-preview endpoint strips ANSI on request, so an agent reading
/// over MCP gets plain text; the default preserves the raw escape stream for
/// callers that render it themselves.
#[tokio::test]
async fn history_log_preview_strips_ansi_when_requested() {
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
            "name": "ansi-me",
            "command": [
                "/bin/sh",
                "-lc",
                "printf '\\033[31mred-needle\\033[0m plain-tail\\n'; exit 0",
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

    // Wait for the finalized row so the raw log is fully flushed.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let session = client
            .get(format!("{base}/api/history/{id}"))
            .send()
            .await
            .unwrap();
        if session.status() == StatusCode::OK {
            let row: Value = session.json().await.unwrap();
            if !row["exit_code"].is_null() {
                break;
            }
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "session was not archived in time"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // Default: raw stream, escapes preserved.
    let raw: Value = client
        .get(format!("{base}/api/history/{id}/log"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let raw_text = raw["text"].as_str().unwrap_or_default();
    assert!(
        raw_text.contains('\u{1b}'),
        "default preview must keep the raw escape stream: {raw:?}"
    );

    // strip_ansi=true: readable plain text, no escape bytes.
    let stripped: Value = client
        .get(format!("{base}/api/history/{id}/log?strip_ansi=true"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let text = stripped["text"].as_str().unwrap_or_default();
    assert!(
        text.contains("red-needle") && text.contains("plain-tail"),
        "stripped preview must keep the visible text: {stripped:?}"
    );
    assert!(
        !text.contains('\u{1b}'),
        "stripped preview must contain no escape bytes: {stripped:?}"
    );
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
                "printf 'file=[%s]\\n' \"$VOGT_ENGINE_AGENT_TASK_PROMPT_FILE\"; \
                 cat \"$VOGT_ENGINE_AGENT_TASK_PROMPT_FILE\""],
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

    let path_line = format!("file=[{}]", prompt_path.display());
    let printed = session_output_after_exit(
        &client,
        &base,
        &id,
        &[&path_line, "Fix the flaky forge test."],
    )
    .await;
    assert!(
        printed.contains(&path_line),
        "child should be told the prompt file path; got {printed:?}"
    );
    assert!(
        printed.contains("Fix the flaky forge test."),
        "child should be able to read the brief; got {printed:?}"
    );
}

/// WI-827/832/833 end to end: an agent CLI started with a brief is handed a
/// first prompt naming the brief file (so it begins the task rather than
/// opening idle), a fresh Claude Code launch is pinned to the engine's
/// session id as its conversation id, and the quiet defaults reach its env.
/// A stub named `claude` prints what it was given, standing in for the CLI.
#[tokio::test]
async fn an_agent_started_with_a_brief_is_told_to_read_it() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = test_config();
    cfg.default_cwd = tmp.path().to_path_buf();
    cfg.workspace_root = tmp.path().canonicalize().unwrap();
    cfg.state_dir = tmp.path().join("state");

    let bin = tmp.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let stub = bin.join("claude");
    std::fs::write(
        &stub,
        // One write, then a pause: the helper below stops reading once the
        // child has exited, so output must not trail the exit.
        "#!/bin/sh\nout=$(printf 'arg=[%s]\\n' \"$@\")\n\
         printf '%s\\nsuggest=[%s]\\n' \"$out\" \"$CLAUDE_CODE_ENABLE_PROMPT_SUGGESTION\"\n\
         sleep 0.3\n",
    )
    .unwrap();
    std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();

    let (base, _h) = boot_with_config(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    let created: Value = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({
            "name": "agent with a task",
            "prompt": "Context.\n\n## Task\n\nCheck the containers.\n",
            "command": [stub.to_string_lossy()],
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

    let printed = session_output_after_exit(&client, &base, &id, &["suggest=["]).await;
    assert!(
        printed.contains(&format!("arg=[--session-id]\r\narg=[{id}]")),
        "a fresh claude launch should carry the session id; got {printed:?}"
    );
    assert!(
        printed.contains(&format!(
            "arg=[Vogt started this session with a brief in {}.",
            prompt_path.display()
        )),
        "the first prompt should name the brief file; got {printed:?}"
    );
    assert!(
        !printed.contains("Check the containers"),
        "the brief's text must not be argv; got {printed:?}"
    );
    assert!(
        printed.contains("suggest=[false]"),
        "prompt suggestions should be off; got {printed:?}"
    );
}

#[tokio::test]
async fn a_resume_for_a_shell_is_refused() {
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
    for (command, resume) in [
        (json!(["bash"]), "0f8fad5b-d9cb-469f-a165-70867728950e"),
        (json!(["claude"]), "--dangerously-skip-permissions"),
    ] {
        let status = client
            .post(format!("{base}/api/sessions"))
            .json(&json!({"name": "r", "command": command, "resume": resume}))
            .send()
            .await
            .unwrap()
            .status();
        assert_eq!(status, 400, "{command} resume {resume}");
    }
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
                "printf 'file=[%s]\\n' \"$VOGT_ENGINE_AGENT_TASK_PROMPT_FILE\""],
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = created["id"].as_str().unwrap().to_string();

    let printed = session_output_after_exit(&client, &base, &id, &["file=[]"]).await;
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
/// The scrollback of a session that has exited, once it holds every string
/// in `expected`.
///
/// The exit code and the PTY reader's last bytes race: on a loaded runner the
/// session can report its exit while the tail of the child's output is still
/// on its way into the scrollback. So this polls until both the exit and the
/// expected content are visible, and on its deadline returns what it has for
/// the caller's assertion to report.
async fn session_output_after_exit(
    client: &reqwest::Client,
    base: &str,
    id: &str,
    expected: &[&str],
) -> String {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        let detail: SessionDetail = client
            .get(format!("{base}/api/sessions/{id}"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let exited = detail.summary.exit_code.is_some() && detail.summary.scrollback_bytes > 0;
        let snapshot = base64::engine::general_purpose::STANDARD
            .decode(detail.scrollback_base64.as_bytes())
            .unwrap();
        let printed = String::from_utf8_lossy(&snapshot).into_owned();
        if exited && expected.iter().all(|want| printed.contains(want)) {
            return printed;
        }
        if tokio::time::Instant::now() >= deadline {
            assert!(
                exited,
                "session {id} never exited with output; exit_code={:?}, scrollback_bytes={}",
                detail.summary.exit_code, detail.summary.scrollback_bytes,
            );
            return printed;
        }
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
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

/// Attach sending a caller-supplied auth frame verbatim, so a test can exercise
/// the cold-attach `snapshot_tail_bytes` hint that the typed helpers do
/// not send.
async fn ws_attach_with_auth(
    base: &str,
    id: &str,
    auth: Value,
) -> tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>> {
    let ws_url = base.replace("http://", "ws://");
    let url = format!("{ws_url}/api/sessions/{id}/attach");
    let (mut ws, _resp) = tokio_tungstenite::connect_async(url).await.unwrap();
    ws.send(Message::Text(auth.to_string().into()))
        .await
        .unwrap();
    ws
}

/// Read one snapshot sequence (`snapshot-start` → binary frames →
/// `snapshot-done`) and report whether it was a reset, how many snapshot bytes
/// were streamed, and the leading bytes of the payload.
///
/// The leading bytes let a caller assert the ground-state alignment invariant
/// the ring guarantees: a snapshot never begins on a UTF-8 continuation byte,
/// and never in the tail of a chopped escape sequence. Up to
/// [`SNAPSHOT_HEAD_BYTES`] are captured — enough to recognise a `\x1b[` CSI
/// introducer at the start (a *complete* sequence, not a fragment) versus a
/// bare `[`/`m`/digit left over from a mid-sequence cut.
const SNAPSHOT_HEAD_BYTES: usize = 64;

#[allow(dead_code)] // `first_bytes` is only read by the large-session tests.
async fn read_snapshot(
    ws: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
) -> (bool, usize, Vec<u8>) {
    let mut reset = false;
    let mut started = false;
    let mut total = 0usize;
    let mut first_bytes: Vec<u8> = Vec::new();
    loop {
        let m = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .expect("snapshot frame arrives")
            .unwrap()
            .unwrap();
        match m {
            Message::Text(s) => {
                let v: Value = serde_json::from_str(&s).unwrap();
                match v["type"].as_str() {
                    Some("snapshot-start") => {
                        started = true;
                        reset = v["reset"].as_bool().unwrap_or(true);
                    }
                    Some("snapshot-done") => break,
                    _ => {}
                }
            }
            Message::Binary(b) if started => {
                if first_bytes.len() < SNAPSHOT_HEAD_BYTES {
                    let take = (SNAPSHOT_HEAD_BYTES - first_bytes.len()).min(b.len());
                    first_bytes.extend_from_slice(&b[..take]);
                }
                total += b.len();
            }
            _ => {}
        }
    }
    (reset, total, first_bytes)
}

/// A snapshot begins in the terminal's ground state: its first byte is never a
/// UTF-8 continuation byte, and if it opens an escape it is a whole CSI/OSC
/// introducer (`\x1b` followed by `[` or `]`), never the tail of one chopped by
/// an overflow cut (which would begin with a bare `[`, a digit, or `m`).
fn assert_ground_state_aligned(first_bytes: &[u8]) {
    if first_bytes.is_empty() {
        return;
    }
    let b0 = first_bytes[0];
    assert!(
        (b0 & 0xC0) != 0x80,
        "snapshot must not begin on a UTF-8 continuation byte; got {b0:#04x}"
    );
    if b0 == 0x1b {
        assert!(
            matches!(first_bytes.get(1), Some(b'[') | Some(b']')),
            "a leading ESC must introduce a complete CSI/OSC sequence, not a \
             fragment; got {:?}",
            &first_bytes[..first_bytes.len().min(4)]
        );
    }
}

/// Drive more than `at_least` bytes of output into a `cat` session's ring, as
/// many short lines, so a later attach has a scrollback bigger than any small
/// tail cap we then request. The lines are deliberately short: real terminal
/// output has frequent newline seams, so a tail cut lands just after a recent
/// newline rather than dropping one enormous line down to nothing.
async fn fill_scrollback(base: &str, id: &str, at_least: usize) {
    let mut ws = ws_attach(base, id).await;
    read_snapshot(&mut ws).await;
    let mut payload = Vec::new();
    while payload.len() < at_least {
        payload.extend_from_slice(b"0123456789\n"); // 11 bytes per line
    }
    let written = payload.len();
    ws.send(Message::Binary(payload.into())).await.unwrap();
    // `cat` echoes each line and the tty echoes the input, so the ring grows to
    // well beyond what we wrote; accumulate until it clears that mark.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let mut seen = 0usize;
    while seen <= written && tokio::time::Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        match tokio::time::timeout(remaining, ws.next()).await {
            Ok(Some(Ok(Message::Binary(b)))) => seen += b.len(),
            Ok(Some(Ok(_))) => {}
            _ => break,
        }
    }
    assert!(
        seen > written,
        "expected the session to echo more than {written} bytes; saw {seen}"
    );
    ws.close(None).await.ok();
}

/// Kill a session so its child (`/bin/cat`, which never sees EOF) exits. The
/// per-session `child.wait()` runs on a blocking task; leaving it pending
/// stalls the test runtime's shutdown, so every test that spawns one cleans up.
async fn kill_session(client: &reqwest::Client, base: &str, id: &str) {
    let _ = client
        .post(format!("{base}/api/sessions/{id}/kill"))
        .send()
        .await;
}

#[tokio::test]
async fn cold_attach_tail_hint_caps_the_snapshot() {
    // A cold attach (no resume_from) that sends a tail hint must get a
    // full reset snapshot bounded to at most that many bytes, not the entire
    // scrollback ring.
    let (base, _h) = boot().await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    let id: String = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({ "name": "tail", "command": ["/bin/cat"] }))
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    const TAIL: usize = 512;
    fill_scrollback(&base, &id, 4 * TAIL).await;

    // Cold reattach with a small tail hint: a reset snapshot, capped.
    let mut cold = ws_attach_with_auth(
        &base,
        &id,
        json!({ "type": "auth", "token": TEST_TOKEN, "snapshot_tail_bytes": TAIL }),
    )
    .await;
    let (reset, len, _head) = read_snapshot(&mut cold).await;
    assert!(reset, "a cold attach is always a full reset");
    assert!(
        len > 0 && len <= TAIL,
        "cold snapshot should be capped at the tail hint ({TAIL}); got {len}"
    );
    cold.close(None).await.ok();
    kill_session(&client, &base, &id).await;
}

#[tokio::test]
async fn warm_attach_over_budget_is_a_bounded_reset() {
    // F1 (WI-125) inverts the old `warm_attach_is_not_narrowed_by_a_tail_hint`.
    // The tail hint is now the replay budget on *every* path. A warm reattach
    // whose delta exceeds the budget no longer floods the whole ring: it is
    // capped to a ground-state tail and reset:true, because the client's xterm
    // cannot keep more than its scrollback anyway.
    let (base, _h) = boot().await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    let id: String = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({ "name": "warm", "command": ["/bin/cat"] }))
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    const TAIL: usize = 512;
    fill_scrollback(&base, &id, 4 * TAIL).await;

    // resume_from = 0 resolves to the whole retained ring as a delta — far more
    // than the 512-byte budget — so F1 caps it to a bounded reset.
    let mut warm = ws_attach_with_auth(
        &base,
        &id,
        json!({
            "type": "auth",
            "token": TEST_TOKEN,
            "resume_from": 0,
            "snapshot_tail_bytes": TAIL,
        }),
    )
    .await;
    let (reset, len, head) = read_snapshot(&mut warm).await;
    assert!(
        reset,
        "an over-budget delta is a bounded reset, not an append"
    );
    assert!(
        len > 0 && len <= TAIL,
        "the over-budget delta must be capped to the tail budget ({TAIL}); got {len}"
    );
    assert_ground_state_aligned(&head);
    warm.close(None).await.ok();
    kill_session(&client, &base, &id).await;
}

#[tokio::test]
async fn warm_attach_within_budget_stays_a_byte_exact_delta() {
    // The companion to the bounded-reset case: a warm reattach whose delta fits
    // the budget is still sent byte-for-byte with reset:false, so an ordinary
    // switch-away/switch-back appends without clearing the terminal.
    let (base, _h) = boot().await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    let id: String = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({
            "name": "warm-small",
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

    // Prime the ring, record the cursor, then produce a small delta.
    let mut ws = ws_attach(&base, &id).await;
    read_snapshot(&mut ws).await;
    ws.send(Message::Binary(b"before-cursor\n".to_vec().into()))
        .await
        .unwrap();
    let _ = collect_binary_until(&mut ws, b"before-cursor", Duration::from_secs(2)).await;
    let cursor = {
        let detail: Value = client
            .get(format!("{base}/api/sessions/{id}"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        detail["scrollback_pos"].as_u64().unwrap()
    };
    ws.send(Message::Binary(b"after-cursor\n".to_vec().into()))
        .await
        .unwrap();
    let _ = collect_binary_until(&mut ws, b"after-cursor", Duration::from_secs(2)).await;
    ws.close(None).await.ok();

    // A generous budget the tiny delta fits inside: the resume stays a delta.
    let mut warm = ws_attach_with_auth(
        &base,
        &id,
        json!({
            "type": "auth",
            "token": TEST_TOKEN,
            "resume_from": cursor,
            "snapshot_tail_bytes": 1024 * 1024,
        }),
    )
    .await;
    let (reset, delta) = read_snapshot_payload(&mut warm).await;
    assert!(!reset, "a delta within budget must not reset the terminal");
    assert!(
        delta
            .windows(b"after-cursor".len())
            .any(|w| w == b"after-cursor"),
        "the delta must carry the post-cursor output"
    );
    assert!(
        !delta
            .windows(b"before-cursor".len())
            .any(|w| w == b"before-cursor"),
        "the delta must exclude pre-cursor output (byte-exact from the cursor)"
    );
    warm.close(None).await.ok();
    kill_session(&client, &base, &id).await;
}

// ---- large-session resume harness (WI-122 / H1) ----
//
// These tests drive real bytes through the PTY reader, the 1024-slot broadcast
// channel and the outbound coalesce path via a load-generating session, then
// exercise resume across a ring the cursor has aged out of, and the in-band
// lag recovery. Two of them pin *today's* flood behaviour with a comment
// pointing at the F1 (WI-125) inversion; the ground-state-alignment,
// byte-exact-delta and no-duplicate assertions hold across that change.

/// A load-generating session command: a bash loop emitting `count` coloured,
/// sequence-numbered lines — a monotonic `SEQNNNNNNNN` spine so a replay can be
/// checked for gaps and duplicates — with a periodic cursor-home frame so the
/// stream carries real CSI sequences, not just plain text. After the burst it
/// idles (`sleep`) so the ring is stable while a test reattaches; every caller
/// reaps it with `kill_session`. Each line is ~65 bytes.
fn load_session_command(count: u32) -> Vec<String> {
    vec![
        "/bin/bash".into(),
        "-c".into(),
        format!(
            "i=0; while [ \"$i\" -lt {count} ]; do \
                 printf '\\033[3%dmSEQ%08d the quick brown fox jumps over the lazy dog\\033[0m\\n' \
                     \"$((i % 8))\" \"$i\"; \
                 i=$((i + 1)); \
                 if [ \"$((i % 64))\" -eq 0 ]; then printf '\\033[H'; fi; \
             done; \
             exec sleep 3600"
        ),
    ]
}

/// Create a session running `command`, returning its id.
async fn create_command_session(
    client: &reqwest::Client,
    base: &str,
    name: &str,
    command: Vec<String>,
) -> String {
    client
        .post(format!("{base}/api/sessions"))
        .json(&json!({ "name": name, "command": command }))
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string()
}

/// How long the load-session helpers wait for the producer. Locally the whole
/// flood lands in about a second; a contended self-hosted runner has been
/// measured an order of magnitude slower, and an 8 s deadline failed there.
const LOAD_DEADLINE: Duration = Duration::from_secs(45);

/// Read only `scrollback_pos`. `tail_bytes=1` keeps each poll from copying,
/// base64-encoding and JSON-parsing the whole multi-MiB ring every 20 ms,
/// which on a slow runner competed with the very producer being waited on.
async fn scrollback_pos(client: &reqwest::Client, base: &str, id: &str) -> u64 {
    let detail: SessionDetail = client
        .get(format!("{base}/api/sessions/{id}?tail_bytes=1"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    detail.scrollback_pos
}

/// Poll `GET /api/sessions/:id` until `scrollback_pos` reaches `target`,
/// returning the observed position. Fails on a deadline so a session that never
/// produces enough fails rather than hanging.
async fn poll_scrollback_at_least(
    client: &reqwest::Client,
    base: &str,
    id: &str,
    target: u64,
) -> u64 {
    let deadline = tokio::time::Instant::now() + LOAD_DEADLINE;
    loop {
        let pos = scrollback_pos(client, base, id).await;
        if pos >= target {
            return pos;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "session {id} produced only {pos} bytes, wanted >= {target}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Wait until `scrollback_pos` stops advancing (two equal reads 80 ms apart),
/// so a test that compares two reattaches resolves both cursors against the
/// same, stable ring. Returns the settled position.
async fn wait_until_scrollback_stable(client: &reqwest::Client, base: &str, id: &str) -> u64 {
    let deadline = tokio::time::Instant::now() + LOAD_DEADLINE;
    let mut last = u64::MAX;
    loop {
        let pos = scrollback_pos(client, base, id).await;
        if pos == last {
            return pos;
        }
        last = pos;
        assert!(
            tokio::time::Instant::now() < deadline,
            "session {id} never stopped producing (last {pos})"
        );
        tokio::time::sleep(Duration::from_millis(80)).await;
    }
}

/// Read one whole snapshot sequence, returning `(reset, full_payload)`. Unlike
/// [`read_snapshot`] this keeps every byte, so a caller can compare two deltas
/// for byte-exactness.
async fn read_snapshot_payload(
    ws: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
) -> (bool, Vec<u8>) {
    let mut reset = false;
    let mut started = false;
    let mut payload = Vec::new();
    loop {
        let m = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .expect("snapshot frame arrives")
            .unwrap()
            .unwrap();
        match m {
            Message::Text(s) => {
                let v: Value = serde_json::from_str(&s).unwrap();
                match v["type"].as_str() {
                    Some("snapshot-start") => {
                        started = true;
                        reset = v["reset"].as_bool().unwrap_or(true);
                    }
                    Some("snapshot-done") => break,
                    _ => {}
                }
            }
            Message::Binary(b) if started => payload.extend_from_slice(&b),
            _ => {}
        }
    }
    (reset, payload)
}

/// One delivered snapshot/resync sequence: whether it reset the terminal and
/// the payload bytes that followed it.
struct SnapSegment {
    reset: bool,
    bytes: Vec<u8>,
}

/// Drain frames, grouping the payload after each `snapshot-start` into its own
/// segment, until no frame arrives for `idle`. Returns the initial snapshot,
/// the live bytes that followed it, and any in-band resync the server sent —
/// each `snapshot-start` opens a new segment, so a resync is a fresh segment.
async fn read_segments_until_idle(
    ws: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    idle: Duration,
) -> Vec<SnapSegment> {
    let mut segs: Vec<SnapSegment> = Vec::new();
    loop {
        match tokio::time::timeout(idle, ws.next()).await {
            Err(_) | Ok(None) | Ok(Some(Err(_))) => break,
            Ok(Some(Ok(m))) => match m {
                Message::Text(s) => {
                    let v: Value = serde_json::from_str(&s).unwrap();
                    if v["type"] == "snapshot-start" {
                        segs.push(SnapSegment {
                            reset: v["reset"].as_bool().unwrap_or(true),
                            bytes: Vec::new(),
                        });
                    }
                }
                Message::Binary(b) => {
                    if let Some(seg) = segs.last_mut() {
                        seg.bytes.extend_from_slice(&b);
                    }
                }
                _ => {}
            },
        }
    }
    segs
}

/// Extract, in order, every complete `SEQ%08d` marker in `bytes`. The producer
/// emits each sequence number exactly once and monotonically, so this is the
/// spine used to detect gaps and duplicates in a replay.
fn seq_numbers(bytes: &[u8]) -> Vec<u32> {
    let mut out = Vec::new();
    let mut i = 0;
    while i + 11 <= bytes.len() {
        if &bytes[i..i + 3] == b"SEQ" && bytes[i + 3..i + 11].iter().all(u8::is_ascii_digit) {
            let n: u32 = std::str::from_utf8(&bytes[i + 3..i + 11])
                .unwrap()
                .parse()
                .unwrap();
            out.push(n);
            i += 11;
        } else {
            i += 1;
        }
    }
    out
}

/// Assert a run of `SEQ` markers is contiguous: strictly increasing by exactly
/// one, no gap and no duplicate. Used on a delta that resumed from a retained
/// cursor, where every intervening line must be present exactly once.
fn assert_seq_spine_contiguous(bytes: &[u8]) {
    let seqs = seq_numbers(bytes);
    assert!(
        seqs.len() > 10,
        "expected a run of SEQ markers in the delta, saw {}",
        seqs.len()
    );
    for w in seqs.windows(2) {
        assert_eq!(
            w[1],
            w[0] + 1,
            "SEQ spine is not contiguous ({} then {}): a gap or duplicate in the delta",
            w[0],
            w[1]
        );
    }
}

#[tokio::test]
async fn large_stale_warm_reattach_is_a_bounded_reset() {
    // H1(a) / F1. Ring 64 KiB. A load session pushes far past capacity, so a
    // cursor recorded early has aged out of the ring by the time we reattach
    // with it. Before F1 `snapshot_for_attach` answered that stale warm
    // reattach with the FULL untrimmed ring (the flood, 4x worse than a cold
    // attach). F1 (WI-125) bounds it: a ground-state-aligned tail of at most
    // the budget, still reset:true.
    const CAP: usize = 64 * 1024;
    const TAIL_BUDGET: usize = 8 * 1024; // the F1 budget, well under CAP
    let (base, _h) = boot_with(CAP).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    let id = create_command_session(&client, &base, "flood", load_session_command(20_000)).await;

    // Record an early cursor, then wait until the ring has advanced several
    // capacities past it so it is unambiguously aged out.
    let early = poll_scrollback_at_least(&client, &base, &id, CAP as u64 / 2).await;
    let _ = poll_scrollback_at_least(&client, &base, &id, early + 4 * CAP as u64).await;

    let mut warm = ws_attach_with_auth(
        &base,
        &id,
        json!({
            "type": "auth",
            "token": TEST_TOKEN,
            "resume_from": early,
            "snapshot_tail_bytes": TAIL_BUDGET,
        }),
    )
    .await;
    let (reset, len, head) = read_snapshot(&mut warm).await;
    assert!(reset, "a cursor aged out of the ring forces a full reset");
    assert!(
        len > 0 && len <= TAIL_BUDGET,
        "F1 bounds the aged-out reattach to the tail budget ({TAIL_BUDGET}); got {len}"
    );
    assert_ground_state_aligned(&head);
    warm.close(None).await.ok();
    kill_session(&client, &base, &id).await;
}

#[tokio::test]
async fn four_mib_ring_stale_reattach_is_ground_state_aligned_and_bounded() {
    // H1(b) / F1. The dimensions prod runs: a 4 MiB ring with ~6.5 MiB pushed.
    // The stale warm reattach is bounded to the tail budget and reset:true,
    // aligned to the terminal's ground state — not the whole 4 MiB ring.
    const CAP: usize = 4 * 1024 * 1024;
    const TAIL_BUDGET: usize = 1024 * 1024;
    let (base, _h) = boot_with(CAP).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    let id =
        create_command_session(&client, &base, "flood-4m", load_session_command(100_000)).await;

    // Push well past the ring, then reattach from position 1 (long aged out).
    let _ = poll_scrollback_at_least(&client, &base, &id, CAP as u64 + CAP as u64 / 2).await;
    let mut warm = ws_attach_with_auth(
        &base,
        &id,
        json!({
            "type": "auth",
            "token": TEST_TOKEN,
            "resume_from": 1,
            "snapshot_tail_bytes": TAIL_BUDGET,
        }),
    )
    .await;
    let (reset, len, head) = read_snapshot(&mut warm).await;
    assert!(reset, "position 1 has long since aged out of a 4 MiB ring");
    assert!(
        len > 0 && len <= TAIL_BUDGET,
        "F1 bounds the aged-out 4 MiB reattach to the tail budget ({TAIL_BUDGET}); got {len}"
    );
    assert_ground_state_aligned(&head);
    warm.close(None).await.ok();
    kill_session(&client, &base, &id).await;
}

#[tokio::test]
async fn retained_cursor_deltas_are_byte_exact_under_load() {
    // H1(b) — byte-exactness half. Two warm reattaches from retained cursors
    // Ca < Cb into the same idle 4 MiB ring: the later delta must equal the
    // earlier delta with its first (Cb-Ca) bytes removed, and its SEQ spine
    // must be contiguous. Proves `snapshot_since` is byte-exact against bytes
    // that really flowed through the PTY reader and broadcast path, with no gap
    // or duplicate at the resume boundary. This assertion survives F1.
    const CAP: usize = 4 * 1024 * 1024;
    let (base, _h) = boot_with(CAP).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    let id = create_command_session(&client, &base, "delta", load_session_command(80_000)).await;
    // Let it settle so both cursors resolve against the same stable ring.
    let _ = poll_scrollback_at_least(&client, &base, &id, CAP as u64 + 256 * 1024).await;
    let total = wait_until_scrollback_stable(&client, &base, &id).await;

    // Both cursors sit inside the retained 4 MiB window.
    let ca = total - 2 * 1024 * 1024;
    let cb = ca + 512 * 1024;

    let mut a = ws_attach_with_token_and_cursor(&base, &id, TEST_TOKEN, Some(ca)).await;
    let (reset_a, delta_a) = read_snapshot_payload(&mut a).await;
    let mut b = ws_attach_with_token_and_cursor(&base, &id, TEST_TOKEN, Some(cb)).await;
    let (reset_b, delta_b) = read_snapshot_payload(&mut b).await;

    assert!(!reset_a && !reset_b, "retained cursors resume as deltas");
    let skip = (cb - ca) as usize;
    assert!(
        delta_a.len() >= skip && delta_a.len() == skip + delta_b.len(),
        "delta lengths inconsistent: |A|={} skip={} |B|={}",
        delta_a.len(),
        skip,
        delta_b.len()
    );
    assert_eq!(
        &delta_a[skip..],
        &delta_b[..],
        "the later delta must equal the earlier delta past the (Cb-Ca) boundary"
    );
    assert_seq_spine_contiguous(&delta_b);

    a.close(None).await.ok();
    b.close(None).await.ok();
    kill_session(&client, &base, &id).await;
}

#[tokio::test]
async fn lagging_subscriber_recovers_in_band_bounded_and_without_duplicates() {
    // H1(c) — lag recovery. A client stops reading while a load session floods
    // far past the 1024-slot broadcast channel, so the server must recover the
    // socket in-band (`send_resync`) rather than drop it. The ring is sized
    // above the ~8 MiB broadcast overflow so the resync stays a reset:false
    // delta. Assert: a resync segment appears, every segment's payload is
    // bounded by the ring, and the SEQ spine across the whole delivered stream
    // is strictly increasing — no duplicate and no reordering across the
    // resync boundary.
    const CAP: usize = 12 * 1024 * 1024;
    let (base, _h) = boot_with(CAP).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    // ~11.7 MiB of output, comfortably past the broadcast channel capacity.
    let id = create_command_session(&client, &base, "lag", load_session_command(180_000)).await;

    let mut ws = ws_attach(&base, &id).await;
    // Do not read at all: the initial snapshot and the live stream both queue
    // in the socket buffer, the outbound task blocks on a full socket, and the
    // broadcast channel overflows behind it. Wait for the producer to finish
    // rather than a fixed 1.5 s: locally the flood is done well inside that,
    // but on a slow runner it was not, the channel never overflowed, and the
    // test saw no resync segment.
    tokio::time::sleep(Duration::from_millis(1500)).await;
    wait_until_scrollback_stable(&client, &base, &id).await;
    // Resume and drain everything, including the in-band resync.
    let segs = read_segments_until_idle(&mut ws, Duration::from_millis(1500)).await;

    assert!(
        segs.len() >= 2,
        "expected an in-band resync segment after the flood; saw {} segment(s)",
        segs.len()
    );
    assert!(
        segs[0].reset,
        "the initial cold attach is always a full reset snapshot"
    );
    for s in &segs {
        assert!(
            s.bytes.len() <= CAP + CAP / 16,
            "a resync/snapshot payload must be bounded by the ring; got {}",
            s.bytes.len()
        );
    }
    // The resync snapshots from the client's last sent position, so it never
    // re-sends bytes already delivered: the spine is strictly increasing.
    let mut all = Vec::new();
    for s in &segs {
        all.extend(seq_numbers(&s.bytes));
    }
    assert!(
        all.len() > 100,
        "expected a long SEQ spine across the delivered stream, saw {}",
        all.len()
    );
    for w in all.windows(2) {
        assert!(
            w[1] > w[0],
            "SEQ spine went backward or duplicated across a resync: {} then {}",
            w[0],
            w[1]
        );
    }

    ws.close(None).await.ok();
    kill_session(&client, &base, &id).await;
}

#[tokio::test]
async fn pong_never_reports_the_server_ahead_of_this_socket() {
    // F2 (WI-126). Under load the liveness pong must carry the position actually
    // streamed to THIS socket, never `total_written` (which includes queued but
    // unsent output). Before the fix the inbound task answered with
    // scrollback_position() and the pong could be delivered ahead of the chunks
    // it referenced, so the client saw the server "ahead" and recycled the
    // socket. Now the outbound task answers with sent_pos after flushing, so the
    // pong's pos is never greater than what this socket has received.
    let (base, _h) = boot_with(4 * 1024 * 1024).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let id = create_command_session(&client, &base, "pong", load_session_command(50_000)).await;

    let mut ws = ws_attach(&base, &id).await;

    // Drain the initial snapshot, recording the absolute position it ended at.
    let mut snap_end: u64 = 0;
    let mut in_snapshot = false;
    loop {
        let m = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .expect("snapshot frame arrives")
            .unwrap()
            .unwrap();
        match m {
            Message::Text(s) => {
                let v: Value = serde_json::from_str(&s).unwrap();
                match v["type"].as_str() {
                    Some("snapshot-start") => {
                        in_snapshot = true;
                        snap_end = v["scrollback_pos"].as_u64().unwrap();
                    }
                    Some("snapshot-done") => break,
                    _ => {}
                }
            }
            Message::Binary(_) if in_snapshot => {}
            _ => {}
        }
    }

    // Probe while output is still flowing.
    ws.send(Message::Text(
        json!({ "type": "ping", "id": 1 }).to_string().into(),
    ))
    .await
    .unwrap();

    // Count live bytes received on this socket until the pong arrives. The pong
    // is ordered after any flushed chunks, so by the time we read it we have
    // received every byte up to its position.
    let mut live: u64 = 0;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let m = tokio::time::timeout(remaining, ws.next())
            .await
            .expect("a pong should arrive within the deadline")
            .unwrap()
            .unwrap();
        match m {
            Message::Binary(b) => live += b.len() as u64,
            Message::Text(s) => {
                let v: Value = serde_json::from_str(&s).unwrap();
                if v["type"] == "pong" {
                    assert_eq!(v["id"].as_u64(), Some(1));
                    let pos = v["pos"].as_u64().unwrap();
                    let received = snap_end + live;
                    assert!(
                        pos <= received,
                        "pong pos {pos} is ahead of what this socket received \
                         ({received} = snap_end {snap_end} + live {live})"
                    );
                    break;
                }
                // A resync snapshot-start would only add to `received`; keep going.
            }
            _ => {}
        }
    }

    ws.close(None).await.ok();
    kill_session(&client, &base, &id).await;
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

    // 2) snapshot-done. A live session's cold attach sends the grid's frame
    // (WI-121), so binary frames may precede it even before any input.
    loop {
        let m = tokio::time::timeout(Duration::from_secs(2), ws.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        if let Message::Text(s) = m {
            if serde_json::from_str::<Value>(&s).unwrap()["type"] == "snapshot-done" {
                break;
            }
        }
    }

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
async fn ws_ping_returns_the_session_output_position() {
    let (base, _h) = boot().await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let id: String = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({
            "name": "ping",
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

    ws.send(Message::Binary(b"ping-output\n".to_vec().into()))
        .await
        .unwrap();
    let _ = collect_binary_until(&mut ws, b"ping-output", Duration::from_secs(2)).await;
    let detail: Value = client
        .get(format!("{base}/api/sessions/{id}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let expected_pos = detail["scrollback_pos"].as_u64().unwrap();

    ws.send(Message::Text(
        json!({"type": "ping", "id": 19}).to_string().into(),
    ))
    .await
    .unwrap();
    let pong = loop {
        let message = tokio::time::timeout(Duration::from_secs(2), ws.next())
            .await
            .expect("pong arrives")
            .unwrap()
            .unwrap();
        if let Message::Text(text) = message {
            let value: Value = serde_json::from_str(&text).unwrap();
            if value["type"] == "pong" {
                break value;
            }
        }
    };
    assert_eq!(pong["id"], 19);
    assert_eq!(pong["pos"], expected_pos);

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
async fn file_upload_streams_to_disk() {
    let tmp = tempfile::tempdir().unwrap();
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

    // A payload comfortably larger than the 2 MiB JSON default, to prove the
    // streaming route carries what the buffered one would reject.
    let payload: Vec<u8> = (0..(5 * 1024 * 1024u32)).map(|i| (i % 251) as u8).collect();

    let r = client
        .put(format!(
            "{base}/api/files/upload?path=up/big.bin&create_parents=true"
        ))
        .body(payload.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let body: Value = r.json().await.unwrap();
    assert_eq!(body["bytes"].as_u64().unwrap(), payload.len() as u64);

    // Bytes landed exactly, and the returned hash is the content SHA-256.
    let on_disk = std::fs::read(tmp.path().join("up/big.bin")).unwrap();
    assert_eq!(on_disk, payload);
    let mut hasher = <sha2::Sha256 as sha2::Digest>::new();
    sha2::Digest::update(&mut hasher, &payload);
    let expect_hash = sha2::Digest::finalize(hasher)
        .iter()
        .fold(String::new(), |mut s, b| {
            use std::fmt::Write as _;
            let _ = write!(s, "{b:02x}");
            s
        });
    assert_eq!(body["hash"].as_str().unwrap(), expect_hash);

    // No spooled temp file is left behind in the target directory.
    let leftovers: Vec<String> = std::fs::read_dir(tmp.path().join("up"))
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        leftovers,
        vec!["big.bin".to_string()],
        "temp file leaked: {leftovers:?}"
    );

    // if_match guards a streaming overwrite just like the JSON write does: a
    // stale baseline is a 409, not a clobber.
    let stale = client
        .put(format!(
            "{base}/api/files/upload?path=up/big.bin&if_match=deadbeef"
        ))
        .body(vec![b'x'; 16])
        .send()
        .await
        .unwrap();
    assert_eq!(stale.status(), StatusCode::CONFLICT);
    assert_eq!(
        std::fs::read(tmp.path().join("up/big.bin")).unwrap(),
        payload
    );
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

// ── a configuration that would hang refuses instead ───────────────────────

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

// ── unprovisioned means invisible, not broken ─────────────────────────────

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

// ── sessions do not depend on the core ────────────────────────────────────

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
         core independence needs its own fixture rather than this assertion deleted"
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

// ── more than one client on one session, at the same time ─────────────────

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
         watchers, which is what concurrent means here"
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

// ── the activity state reaches the server-wide event stream ───────────────

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
    // This has two halves — the state is *derived from output heuristics*,
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

// ── the notifications that are worth a phone interruption ─────────────────

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
/// the value the requirement gives it and an omitted `errored` would silently be `true`.
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
    // The headline notification case, and the one the drift watcher's unit tests do
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
    // The third of the named notification events, and the only one that comes from a
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
                "sleep 0.3; printf 'VOGT_NOTIFY:two references are dangling\\n'"],
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

// -- Server-side speech --------------------------------------------

/// Unconfigured, both speech routes 404 so the client falls back. The
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
    // A caller the core resolves with `read` only: may not drive the
    // assistant. The front door learns that from the core's scopes.
    let core = stand_in_core_knowing(vec![(
        "reader-token-1234567890abcdef",
        "human:reader",
        vec!["read"],
    )])
    .await;
    let mut cfg = test_config();
    cfg.vogt_core_url = Some(core);
    let (base, _h) = boot_with_config(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth_for("reader-token-1234567890abcdef"))
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

// --- fail-closed approval gates and mid-run steering -----------------------

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

// --- typed outcomes, the conclusion record, schema-validated findings -------

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
/// the diff stats for what the run left on it.
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
/// the fail-closed path surfaced as the typed verdict.
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

// ── On-demand secret broker ───────────────────────────────────────

/// A stand-in for `vogt-agent-auth get VAR`. It answers with a value
/// that names the variable, fails for `BROKEN` exactly as the reference
/// helper does for an empty secret, and refuses every other subcommand with
/// a distinctive status — so if the engine ever ran anything but `get` here,
/// the test would say so.
///
/// It also answers `set VAR`: `FAILWRITE` fails with a distinctive
/// stderr, and otherwise it records the value it read on stdin, its argv and
/// its whole environment into files under `dir` — so a test can prove the value
/// arrived on stdin and nowhere else — then prints `created`.
fn fake_broker_helper(dir: &std::path::Path) -> std::path::PathBuf {
    let helper = dir.join("agent-auth");
    let rec = dir.display();
    std::fs::write(
        &helper,
        format!(
            "#!/bin/sh\n\
             case \"$1\" in\n\
             get)\n\
             [ \"$2\" = BROKEN ] && {{ echo 'vogt-agent-auth: Infisical secret broken is missing or empty' >&2; exit 1; }}\n\
             printf 'value-of-%s' \"$2\" ;;\n\
             set)\n\
             [ \"$2\" = FAILWRITE ] && {{ echo 'vogt-agent-auth: upstream refused the write' >&2; exit 1; }}\n\
             cat > '{rec}/store-stdin'\n\
             printf '%s' \"$*\" > '{rec}/store-argv'\n\
             env > '{rec}/store-env'\n\
             echo created ;;\n\
             *) echo \"unexpected subcommand: $*\" >&2; exit 64 ;;\n\
             esac\n"
        ),
    )
    .unwrap();
    let mut permissions = std::fs::metadata(&helper).unwrap().permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(&helper, permissions).unwrap();
    helper
}

fn broker_config(tmp: &tempfile::TempDir, manifest: &str) -> Config {
    let mut cfg = test_config();
    cfg.default_cwd = tmp.path().to_path_buf();
    cfg.workspace_root = tmp.path().canonicalize().unwrap();
    cfg.agent_auth_helper = fake_broker_helper(tmp.path());
    cfg.agent_auth_secrets = vogt_engine_server::secret_broker::parse_manifest(manifest).unwrap();
    cfg
}

async fn create_session_id(client: &reqwest::Client, base: &str, name: &str) -> String {
    client
        .post(format!("{base}/api/sessions"))
        .json(&json!({ "name": name }))
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string()
}

#[tokio::test]
async fn secret_broker_hands_a_session_one_declared_secret_and_only_that() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = broker_config(
        &tmp,
        "LAUNCHED proj launched\nLATER proj later ondemand\nBROKEN proj broken ondemand\n",
    );
    let (base, state, _h) = boot_with_state(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let id = create_session_id(&client, &base, "broker").await;
    let session: uuid::Uuid = id.parse().unwrap();
    let token = state.sessions.secret_broker().issue(session);
    // A session, not an operator: no engine bearer on this client.
    let plain = reqwest::Client::new();
    let fetch = |var: &str| format!("{base}/api/agent-auth/fetch/{var}");

    // The declared, on-demand name: the value, verbatim, marked no-store.
    let r = plain
        .post(fetch("LATER"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(r.headers()["cache-control"], "no-store");
    assert_eq!(r.text().await.unwrap(), "value-of-LATER");

    // Policy is manifest membership, not only `ondemand`.
    let r = plain
        .post(fetch("LAUNCHED"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(r.text().await.unwrap(), "value-of-LAUNCHED");

    // An undeclared name is refused with the manifest line to add — the
    // message a session sees is the fix, not "credential missing".
    let r = plain
        .post(fetch("NOPE"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 403);
    let error = r.json::<Value>().await.unwrap()["error"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(error.contains("NOPE"), "{error}");
    assert!(error.contains("ENGINE_AGENT_AUTH_SECRETS"), "{error}");
    assert!(error.contains("ondemand"), "names the flag to use: {error}");

    // A helper failure is reported as the upstream's own reason, never as a
    // value and never as an engine fault.
    let r = plain
        .post(fetch("BROKEN"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 502);
    let error = r.json::<Value>().await.unwrap()["error"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(error.contains("missing or empty"), "{error}");

    // Only the session's own token opens the broker: not a made-up one, not
    // none, and not the engine bearer — a token that can create sessions has
    // no business reading their secrets.
    for (label, request) in [
        (
            "wrong token",
            plain.post(fetch("LATER")).bearer_auth("not-it"),
        ),
        ("no token", plain.post(fetch("LATER"))),
        (
            "engine bearer",
            plain.post(fetch("LATER")).bearer_auth(TEST_TOKEN),
        ),
    ] {
        let r = request.send().await.unwrap();
        assert_eq!(r.status(), 401, "{label}");
    }

    // Forgetting the session forgets its leave to ask.
    assert!(client
        .delete(format!("{base}/api/sessions/{id}"))
        .send()
        .await
        .unwrap()
        .status()
        .is_success());
    let r = plain
        .post(fetch("LATER"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401, "revoked with the session");
}

/// WI-973: a person-approved credential grant, as the engine holds and
/// honours it. Only vogt-core's identity may apply or revoke one; it reaches
/// only the session it was granted to; `once` is consumed by the first fetch;
/// `ttl` stands until revoked or expired; and a grant never shadows the
/// manifest or reaches a project the operator has not opened.
#[tokio::test]
async fn an_approved_grant_reaches_only_its_session_until_revoked_or_expired() {
    const STACK: &str = "stack-secret-for-grants-1234567890";
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = broker_config(&tmp, "LATER proj later ondemand\n");
    // A helper that also says which manifest it was run with, so the test
    // sees the granted line replace the deployment's for the call.
    let helper = tmp.path().join("grant-helper");
    std::fs::write(
        &helper,
        "#!/bin/sh\n[ \"$1\" = get ] || exit 64\n[ \"$2\" = GRANT_BROKEN ] && exit 3\nprintf 'value-of-%s|%s' \"$2\" \"$ENGINE_AGENT_AUTH_SECRETS\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o700)).unwrap();
    cfg.agent_auth_helper = helper;
    cfg.vogt_core_token = Some(STACK.into());
    cfg.agent_grant_projects = vec!["extra".into()];
    let (base, state, _h) = boot_with_state(cfg).await;
    let operator = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let core = reqwest::Client::builder()
        .default_headers(auth_for(STACK))
        .build()
        .unwrap();
    let plain = reqwest::Client::new();
    let target = create_session_id(&operator, &base, "worker").await;
    let other = create_session_id(&operator, &base, "bystander").await;
    let target_token = state
        .sessions
        .secret_broker()
        .issue(target.parse().unwrap());
    let other_token = state.sessions.secret_broker().issue(other.parse().unwrap());
    let fetch = |token: &str, var: &str| {
        plain
            .post(format!("{base}/api/agent-auth/fetch/{var}"))
            .bearer_auth(token.to_string())
            .send()
    };
    let in_an_hour = (OffsetDateTime::now_utc() + time::Duration::hours(1))
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap();
    let grant = |id: &str, var: &str, project: &str, uses: &str, expires: &str| {
        json!({
            "grant_id": id,
            "var": var,
            "project_id": project,
            "secret_name": "100.109.218.11_SSH",
            "uses": uses,
            "expires_at": expires,
            "reason": "the worker needs the emulator key",
        })
    };

    // Deny by default: nothing granted, nothing fetched.
    assert_eq!(
        fetch(&target_token, "GRANT_SSH").await.unwrap().status(),
        403
    );

    // Only vogt-core applies a grant: the break-glass operator token holds
    // `sessions` and is still refused.
    let refused = operator
        .post(format!("{base}/api/sessions/{target}/grants"))
        .json(&grant("grt_1", "GRANT_SSH", "proj", "once", &in_an_hour))
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), 403);
    assert!(refused.text().await.unwrap().contains("only vogt-core"));

    let applied = core
        .post(format!("{base}/api/sessions/{target}/grants"))
        .json(&grant("grt_1", "GRANT_SSH", "proj", "once", &in_an_hour))
        .send()
        .await
        .unwrap();
    assert_eq!(applied.status(), 200, "{:?}", applied.text().await);

    // It reaches only its own session.
    assert_eq!(
        fetch(&other_token, "GRANT_SSH").await.unwrap().status(),
        403
    );
    let got = fetch(&target_token, "GRANT_SSH").await.unwrap();
    assert_eq!(got.status(), 200);
    assert_eq!(
        got.text().await.unwrap(),
        "value-of-GRANT_SSH|GRANT_SSH proj 100.109.218.11_SSH ondemand",
        "the helper sees exactly the granted line as its manifest"
    );
    // `once`: consumed by that fetch.
    assert_eq!(
        fetch(&target_token, "GRANT_SSH").await.unwrap().status(),
        403
    );

    // A standing (ttl) grant answers until it is revoked; the session can list
    // it, never with a value.
    core.post(format!("{base}/api/sessions/{target}/grants"))
        .json(&grant("grt_2", "GRANT_SSH", "extra", "ttl", &in_an_hour))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    for _ in 0..2 {
        assert_eq!(
            fetch(&target_token, "GRANT_SSH").await.unwrap().status(),
            200
        );
    }
    let listed: Vec<Value> = plain
        .get(format!("{base}/api/agent-auth/grants"))
        .bearer_auth(&target_token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0]["grant_id"], "grt_2");
    assert_eq!(
        listed[0]["reason"], "the worker needs the emulator key",
        "the session sees the purpose the person approved"
    );
    assert!(!listed[0].to_string().contains("value-of"));
    // One approval never silently ends another: a different grant for a var
    // the session can already fetch is refused, while re-applying the same
    // grant (a retried approval) replaces it.
    let in_use = core
        .post(format!("{base}/api/sessions/{target}/grants"))
        .json(&grant("grt_2b", "GRANT_SSH", "proj", "once", &in_an_hour))
        .send()
        .await
        .unwrap();
    assert_eq!(in_use.status(), 409);
    assert!(in_use
        .text()
        .await
        .unwrap()
        .contains("already holds a live grant"));
    core.post(format!("{base}/api/sessions/{target}/grants"))
        .json(&grant("grt_2", "GRANT_SSH", "extra", "ttl", &in_an_hour))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    // An operator reads a session's grants; the reason rides along.
    let seen: Vec<Value> = operator
        .get(format!("{base}/api/sessions/{target}/grants"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(seen.len(), 1, "{seen:?}");
    assert_eq!(seen[0]["grant_id"], "grt_2");
    let empty: Vec<Value> = plain
        .get(format!("{base}/api/agent-auth/grants"))
        .bearer_auth(&other_token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(empty.is_empty());
    assert_eq!(
        operator
            .delete(format!("{base}/api/sessions/{target}/grants/grt_2"))
            .send()
            .await
            .unwrap()
            .status(),
        403,
        "revoking is vogt-core's too"
    );
    core.delete(format!("{base}/api/sessions/{target}/grants/grt_2"))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    assert_eq!(
        fetch(&target_token, "GRANT_SSH").await.unwrap().status(),
        403
    );

    // A `once` grant is spent by its first fetch even when the helper fails:
    // taken before the helper runs and never put back, so a revoke landing
    // mid-fetch is not undone by the failure.
    core.post(format!("{base}/api/sessions/{target}/grants"))
        .json(&grant(
            "grt_broken",
            "GRANT_BROKEN",
            "proj",
            "once",
            &in_an_hour,
        ))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    assert_eq!(
        fetch(&target_token, "GRANT_BROKEN").await.unwrap().status(),
        502,
        "the helper failed"
    );
    assert_eq!(
        fetch(&target_token, "GRANT_BROKEN").await.unwrap().status(),
        403,
        "a failed once-fetch still spent the grant"
    );
    let after_failure: Vec<Value> = plain
        .get(format!("{base}/api/agent-auth/grants"))
        .bearer_auth(&target_token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        after_failure.iter().all(|g| g["grant_id"] != "grt_broken"),
        "{after_failure:?}"
    );

    // Least privilege: never a manifest name, never an unopened project,
    // never past 24 h or already expired.
    let too_late = (OffsetDateTime::now_utc() + time::Duration::days(2))
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap();
    for (body, status) in [
        (grant("g", "LATER", "proj", "ttl", &in_an_hour), 409),
        (
            grant("g", "GRANT_SSH", "elsewhere", "ttl", &in_an_hour),
            403,
        ),
        (grant("g", "GRANT_SSH", "proj", "ttl", &too_late), 400),
        (
            grant("g", "GRANT_SSH", "proj", "ttl", "2020-01-01T00:00:00Z"),
            400,
        ),
        (grant("g", "not a var", "proj", "ttl", &in_an_hour), 400),
    ] {
        let r = core
            .post(format!("{base}/api/sessions/{target}/grants"))
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), status, "{body}");
    }

    // Expiry is enforced at use, not only shown.
    let soon = (OffsetDateTime::now_utc() + time::Duration::milliseconds(1500))
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap();
    core.post(format!("{base}/api/sessions/{target}/grants"))
        .json(&grant("grt_3", "GRANT_SSH", "proj", "ttl", &soon))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    assert_eq!(
        fetch(&target_token, "GRANT_SSH").await.unwrap().status(),
        200
    );
    tokio::time::sleep(Duration::from_millis(1700)).await;
    assert_eq!(
        fetch(&target_token, "GRANT_SSH").await.unwrap().status(),
        403
    );

    // The session ending takes its grants with it.
    core.post(format!("{base}/api/sessions/{target}/grants"))
        .json(&grant("grt_4", "GRANT_SSH", "proj", "ttl", &in_an_hour))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    operator
        .post(format!("{base}/api/sessions/{target}/kill"))
        .send()
        .await
        .unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let held: Vec<Value> = operator
            .get(format!("{base}/api/sessions/{target}/grants"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap_or_default();
        if held.is_empty() {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "grants outlived the session"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let late = core
        .post(format!("{base}/api/sessions/{target}/grants"))
        .json(&grant("grt_5", "GRANT_SSH", "proj", "ttl", &in_an_hour))
        .send()
        .await
        .unwrap();
    assert_eq!(late.status(), 409, "no grant for an exited session");
}

#[tokio::test]
async fn a_session_is_handed_the_token_the_broker_honours() {
    // End to end through the real spawn: the token that lands in the child's
    // environment is the one the broker authenticates as that session — and
    // it lands in a caller-command session too, not only the auto-agent-auth
    // shell, because the policy is the manifest, not the launch path.
    let tmp = tempfile::tempdir().unwrap();
    let cfg = broker_config(&tmp, "LATER proj later ondemand\n");
    let (base, state, _h) = boot_with_state(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let out = tmp.path().join("child-env");
    let id = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({
            "name": "reads-its-grant",
            "command": ["/bin/sh", "-c",
                "printenv VOGT_ENGINE_BROKER_TOKEN > \"$OUT\"; printenv VOGT_ENGINE_BROKER_URL >> \"$OUT\"; sleep 30"],
            "env": [["OUT", out.to_string_lossy()]],
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
    let session: uuid::Uuid = id.parse().unwrap();

    let lines = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Ok(text) = std::fs::read_to_string(&out) {
                let lines: Vec<String> = text.lines().map(str::to_string).collect();
                if lines.len() >= 2 {
                    break lines;
                }
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("the child should write both broker variables");
    let (token, url) = (&lines[0], &lines[1]);
    assert_eq!(token.len(), 64, "two simple v4 uuids of randomness");
    assert!(
        url.starts_with("http://127.0.0.1:"),
        "the engine on loopback: {url}"
    );
    assert_eq!(
        state.sessions.secret_broker().authenticate(token),
        Some(session),
        "the token the child holds is this session's"
    );
    let r = reqwest::Client::new()
        .post(format!("{base}/api/agent-auth/fetch/LATER"))
        .bearer_auth(token)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(r.text().await.unwrap(), "value-of-LATER");

    let _ = client
        .delete(format!("{base}/api/sessions/{id}"))
        .send()
        .await;
}

#[tokio::test]
async fn secret_store_writes_a_writable_secret_on_stdin_only() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = broker_config(
        &tmp,
        "RW proj rw ondemand,writable\nRO proj ro ondemand\nFAILWRITE proj fw ondemand,writable\n",
    );
    let recorded = tmp.path().to_path_buf();
    let (base, state, _h) = boot_with_state(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let id = create_session_id(&client, &base, "store").await;
    let session: uuid::Uuid = id.parse().unwrap();
    let token = state.sessions.secret_broker().issue(session);
    let plain = reqwest::Client::new();
    let store = |var: &str| format!("{base}/api/agent-auth/store/{var}");

    // A declared, writable name: 200, and the helper reports what it did.
    let secret = "line1\nline2 super-secret-value";
    let r = plain
        .post(store("RW"))
        .bearer_auth(&token)
        .body(secret)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(r.headers()["cache-control"], "no-store");
    assert_eq!(r.text().await.unwrap(), "created");
    // The value reached the helper on stdin — and nowhere else.
    assert_eq!(
        std::fs::read_to_string(recorded.join("store-stdin")).unwrap(),
        secret
    );
    let argv = std::fs::read_to_string(recorded.join("store-argv")).unwrap();
    assert_eq!(argv, "set RW");
    assert!(
        !argv.contains("super-secret-value"),
        "value must not appear in argv: {argv}"
    );
    let child_env = std::fs::read_to_string(recorded.join("store-env")).unwrap();
    assert!(
        !child_env.contains("super-secret-value"),
        "value must not appear in the child environment"
    );

    // Declared-but-not-writable and undeclared are two distinct 403s.
    let r = plain
        .post(store("RO"))
        .bearer_auth(&token)
        .body("x")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 403);
    let err = r.json::<Value>().await.unwrap()["error"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(err.contains("not writable"), "{err}");

    let r = plain
        .post(store("NOPE"))
        .bearer_auth(&token)
        .body("x")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 403);
    let err = r.json::<Value>().await.unwrap()["error"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(err.contains("ENGINE_AGENT_AUTH_SECRETS"), "{err}");
    assert!(
        err.contains("writable"),
        "suggests the writable flag: {err}"
    );

    // A helper failure is a bad gateway carrying the upstream reason.
    let r = plain
        .post(store("FAILWRITE"))
        .bearer_auth(&token)
        .body("x")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 502);
    let err = r.json::<Value>().await.unwrap()["error"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(err.contains("refused the write"), "{err}");

    // An empty body stores nothing.
    let r = plain
        .post(store("RW"))
        .bearer_auth(&token)
        .body("")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 400);

    // Only the session's own token opens the route; the engine bearer does not.
    for (label, request) in [
        (
            "wrong token",
            plain.post(store("RW")).bearer_auth("not-it").body("x"),
        ),
        ("no token", plain.post(store("RW")).body("x")),
        (
            "engine bearer",
            plain.post(store("RW")).bearer_auth(TEST_TOKEN).body("x"),
        ),
    ] {
        assert_eq!(request.send().await.unwrap().status(), 401, "{label}");
    }

    // Forgetting the session forgets its leave to write.
    assert!(client
        .delete(format!("{base}/api/sessions/{id}"))
        .send()
        .await
        .unwrap()
        .status()
        .is_success());
    let r = plain
        .post(store("RW"))
        .bearer_auth(&token)
        .body("x")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401, "revoked with the session");
}

#[tokio::test]
async fn secret_store_caps_the_body_over_64_kib() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = broker_config(&tmp, "RW proj rw ondemand,writable\n");
    let (base, state, _h) = boot_with_state(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let id = create_session_id(&client, &base, "store-big").await;
    let token = state.sessions.secret_broker().issue(id.parse().unwrap());

    // Over the 64 KiB cap: refused before the helper runs. The body is drained
    // so the client gets a clean 413 rather than deadlocking mid-write.
    let r = reqwest::Client::new()
        .post(format!("{base}/api/agent-auth/store/RW"))
        .bearer_auth(&token)
        .body(vec![b'x'; 64 * 1024 + 1])
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 413);

    let _ = client
        .delete(format!("{base}/api/sessions/{id}"))
        .send()
        .await;
}

#[tokio::test]
async fn secret_store_rate_limits_tighter_than_fetch() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = broker_config(&tmp, "RW proj rw ondemand,writable\n");
    let (base, state, _h) = boot_with_state(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let store = format!("{base}/api/agent-auth/store/RW");

    // The store limit is far below the 60/min fetch limit: a session's writes
    // are refused within the first eleven calls, with a Retry-After.
    let id = create_session_id(&client, &base, "store-rate").await;
    let token = state.sessions.secret_broker().issue(id.parse().unwrap());
    let plain = reqwest::Client::new();
    let mut tripped_at = None;
    for i in 0..12u32 {
        let r = plain
            .post(&store)
            .bearer_auth(&token)
            .body("v")
            .send()
            .await
            .unwrap();
        if r.status() == 429 {
            assert!(
                r.headers().contains_key("retry-after"),
                "a 429 carries Retry-After"
            );
            tripped_at = Some(i);
            break;
        }
        assert_eq!(r.status(), 200, "call {i} is under the limit");
    }
    let tripped_at = tripped_at.expect("the store rate limit should trip");
    assert!(
        tripped_at <= 10,
        "tighter than fetch's 60/min: tripped at call {tripped_at}"
    );

    let _ = client
        .delete(format!("{base}/api/sessions/{id}"))
        .send()
        .await;
}

#[tokio::test]
async fn secret_broker_is_an_honest_absence_when_nothing_is_declared() {
    // An empty manifest is a reported state, not a fault. No session
    // is handed a token, and the route says why it refuses.
    let (base, state, _h) = boot_with_state(test_config()).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let id = create_session_id(&client, &base, "no-broker").await;
    let session: uuid::Uuid = id.parse().unwrap();
    assert!(state.sessions.secret_broker().grant(session).is_none());
    let r = reqwest::Client::new()
        .post(format!("{base}/api/agent-auth/fetch/ANY"))
        .bearer_auth("whatever")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 503);
    let error = r.json::<Value>().await.unwrap()["error"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(error.contains("ENGINE_AGENT_AUTH_SECRETS"), "{error}");
    let _ = client
        .delete(format!("{base}/api/sessions/{id}"))
        .send()
        .await;
}

#[tokio::test]
async fn secret_broker_rate_limits_a_session_that_asks_in_a_loop() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = broker_config(&tmp, "LATER proj later ondemand\n");
    let (base, state, _h) = boot_with_state(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let id = create_session_id(&client, &base, "loop").await;
    let token = state.sessions.secret_broker().issue(id.parse().unwrap());
    let plain = reqwest::Client::new();
    for _ in 0..60 {
        let r = plain
            .post(format!("{base}/api/agent-auth/fetch/LATER"))
            .bearer_auth(&token)
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 200);
    }
    let r = plain
        .post(format!("{base}/api/agent-auth/fetch/LATER"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 429);
    assert!(r.headers().contains_key("retry-after"));
    let _ = client
        .delete(format!("{base}/api/sessions/{id}"))
        .send()
        .await;
}

// ── Runtime-pinned agent CLIs ───────────────────────────────────────
//
// Driven through the *real* installer script (`engine/deploy/agent-cli-
// install.sh`) with a stand-in `npm` on its PATH, so what is asserted is the
// contract the entrypoint relies on too — not a mock of it.

mod agent_clis {
    use super::*;
    use vogt_engine_server::agent_clis::AgentCliPaths;

    const FAKE_NPM: &str = r#"#!/usr/bin/env bash
set -euo pipefail
case "$1" in
  install)
    prefix=""; spec=""
    shift
    while (( $# )); do
      case "$1" in
        --prefix) prefix="$2"; shift ;;
        -g|--global) ;;
        *) spec="$1" ;;
      esac
      shift
    done
    pkg="${spec%@*}"; ver="${spec##*@}"
    if [[ "$ver" == "${FAKE_NPM_FAIL_VERSION:-}" ]]; then
      echo "npm ERR! 404 Not Found - $spec" >&2
      exit 1
    fi
    case "$pkg" in
      @anthropic-ai/claude-code) bin=claude ;;
      @openai/codex) bin=codex ;;
      *) echo "unexpected package $pkg" >&2; exit 2 ;;
    esac
    mkdir -p "$prefix/bin" "$prefix/lib/node_modules/$pkg"
    printf '#!/usr/bin/env bash\necho "%s %s"\n' "$bin" "$ver" > "$prefix/bin/$bin"
    chmod +x "$prefix/bin/$bin"
    printf '{"name":"%s","version":"%s"}\n' "$pkg" "$ver" > "$prefix/lib/node_modules/$pkg/package.json"
    ;;
  view)
    echo "${FAKE_NPM_LATEST:-9.9.9}"
    ;;
  *)
    echo "fake npm: unsupported $*" >&2
    exit 2
    ;;
esac
"#;

    /// A root, a tool table, a baked manifest and an installer wrapper that
    /// runs the real script with the fake npm first on PATH.
    fn sandbox() -> (tempfile::TempDir, AgentCliPaths) {
        let tmp = tempfile::tempdir().unwrap();
        let tools_dir = tmp.path().join("tools");
        std::fs::create_dir_all(&tools_dir).unwrap();
        let npm = tools_dir.join("npm");
        std::fs::write(&npm, FAKE_NPM).unwrap();
        std::fs::set_permissions(&npm, std::fs::Permissions::from_mode(0o755)).unwrap();

        let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("deploy")
            .join("agent-cli-install.sh");
        let installer = tmp.path().join("vogt-agent-cli-install");
        std::fs::write(
            &installer,
            format!(
                "#!/usr/bin/env bash\nexport PATH=\"{}:$PATH\"\nexec bash {} \"$@\"\n",
                tools_dir.display(),
                script.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&installer, std::fs::Permissions::from_mode(0o755)).unwrap();

        let table = tmp.path().join("agent-clis.tools");
        std::fs::write(
            &table,
            "codex\t@openai/codex\tcodex\tVOGT_CODEX_VERSION\n\
             claude-code\t@anthropic-ai/claude-code\tclaude\tVOGT_CLAUDE_CODE_VERSION\n",
        )
        .unwrap();
        let baked = tmp.path().join("agent-versions.resolved");
        std::fs::write(&baked, "codex=0.149.1\nclaude-code=2.1.258\n").unwrap();
        let image_bin = tmp.path().join("image-bin");
        std::fs::create_dir_all(&image_bin).unwrap();
        std::fs::write(
            image_bin.join("claude"),
            "#!/usr/bin/env bash\necho image\n",
        )
        .unwrap();

        let paths = AgentCliPaths {
            root: tmp.path().join("agent-clis"),
            installer,
            tools: table,
            baked,
            image_bin,
        };
        (tmp, paths)
    }

    use std::os::unix::fs::PermissionsExt;

    async fn boot_sandboxed() -> (tempfile::TempDir, String, ServerGuard) {
        let (tmp, paths) = sandbox();
        let mut cfg = test_config();
        cfg.agent_clis = paths;
        // A caller the core resolves as a writer: sessions, files, git — but
        // not the operator grant that moves a CLI pin.
        let core = stand_in_core_knowing(vec![(
            "sessions-only-token-1234567890",
            "human:writer",
            vec!["read", "work.write"],
        )])
        .await;
        cfg.vogt_core_url = Some(core);
        let (base, handle) = boot_with_config(cfg).await;
        (tmp, base, handle)
    }

    fn client() -> reqwest::Client {
        reqwest::Client::builder()
            .default_headers(auth())
            .build()
            .unwrap()
    }

    fn tool<'a>(report: &'a Value, name: &str) -> &'a Value {
        report["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["tool"] == name)
            .unwrap_or_else(|| panic!("no tool {name} in {report}"))
    }

    #[tokio::test]
    async fn the_report_names_the_baked_baseline_as_active_until_a_pin_is_applied() {
        let (_tmp, base, _h) = boot_sandboxed().await;
        let unauth = reqwest::get(format!("{base}/api/agent-clis"))
            .await
            .unwrap();
        assert_eq!(unauth.status(), StatusCode::UNAUTHORIZED);

        let report: Value = client()
            .get(format!("{base}/api/agent-clis"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(report["installer_present"], true);
        let claude = tool(&report, "claude-code");
        assert_eq!(claude["source"], "image");
        assert_eq!(claude["active_version"], "2.1.258");
        assert_eq!(claude["baked_version"], "2.1.258");
        assert_eq!(claude["env_var"], "VOGT_CLAUDE_CODE_VERSION");
        assert!(claude.get("upstream_latest").is_none(), "not asked for");
        // Codex has no image binary in this sandbox: the honest word is absent.
        assert_eq!(tool(&report, "codex")["source"], "absent");
    }

    #[tokio::test]
    async fn a_post_installs_the_version_and_the_report_follows() {
        let (tmp, base, _h) = boot_sandboxed().await;
        let report: Value = client()
            .post(format!("{base}/api/agent-clis/claude-code"))
            .json(&json!({ "version": "2.1.261" }))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap();
        let claude = tool(&report, "claude-code");
        assert_eq!(claude["source"], "runtime");
        assert_eq!(claude["active_version"], "2.1.261");
        assert_eq!(claude["baked_version"], "2.1.258");
        assert_eq!(claude["installed_versions"], json!(["2.1.261"]));
        // The installer's own artefacts are what the report read.
        let root = tmp.path().join("agent-clis");
        assert!(root
            .join("claude-code")
            .join("2.1.261")
            .join("bin")
            .join("claude")
            .exists());
        assert_eq!(
            std::fs::read_to_string(root.join("manifest")).unwrap(),
            "codex=0.149.1\nclaude-code=2.1.261\n"
        );

        // Back to the image copy: offline, and the report says image again.
        let report: Value = client()
            .post(format!("{base}/api/agent-clis/claude-code"))
            .json(&json!({ "version": "image" }))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap();
        let claude = tool(&report, "claude-code");
        assert_eq!(claude["source"], "image");
        assert_eq!(claude["active_version"], "2.1.258");
        // The prefix stays for an offline switch later.
        assert_eq!(claude["installed_versions"], json!(["2.1.261"]));
    }

    #[tokio::test]
    async fn a_failed_install_is_a_conflict_and_changes_nothing() {
        let (tmp, base, _h) = boot_sandboxed().await;
        // The installer inherits the engine's environment; the failing version
        // is named through the fake npm's own switch, set in the wrapper.
        let installer = tmp.path().join("vogt-agent-cli-install");
        let wrapper = std::fs::read_to_string(&installer).unwrap();
        std::fs::write(
            &installer,
            wrapper.replace(
                "exec bash",
                "export FAKE_NPM_FAIL_VERSION=2.1.999\nexec bash",
            ),
        )
        .unwrap();

        let response = client()
            .post(format!("{base}/api/agent-clis/claude-code"))
            .json(&json!({ "version": "2.1.999" }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT);
        let said = response.text().await.unwrap();
        assert!(said.contains("previous version stays"), "{said}");
        assert!(
            said.contains("npm install failed") || said.contains("stays on"),
            "{said}"
        );

        let report: Value = client()
            .get(format!("{base}/api/agent-clis"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(tool(&report, "claude-code")["source"], "image");
        assert!(!tmp
            .path()
            .join("agent-clis")
            .join("claude-code")
            .join("2.1.999")
            .exists());
    }

    #[tokio::test]
    async fn malformed_requests_are_refused_before_the_installer_runs() {
        let (tmp, base, _h) = boot_sandboxed().await;
        for bad in ["2.1", "v2.1.261", "2.1.261; rm -rf /", "--flag", ""] {
            let response = client()
                .post(format!("{base}/api/agent-clis/claude-code"))
                .json(&json!({ "version": bad }))
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{bad:?}");
        }
        // A dist-tag reaches the installer, which refuses it without the opt-in
        // — the installer's EX_USAGE is a bad request here too.
        let response = client()
            .post(format!("{base}/api/agent-clis/claude-code"))
            .json(&json!({ "version": "latest" }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert!(response
            .text()
            .await
            .unwrap()
            .contains("VOGT_AGENT_CLI_ALLOW_DIST_TAGS"));

        let response = client()
            .post(format!("{base}/api/agent-clis/opencode"))
            .json(&json!({ "version": "1.0.0" }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert!(!tmp.path().join("agent-clis").join("opencode").exists());
    }

    #[tokio::test]
    async fn changing_a_pin_needs_the_agent_clis_write_capability() {
        let (_tmp, base, _h) = boot_sandboxed().await;
        let scoped = reqwest::Client::builder()
            .default_headers(auth_for("sessions-only-token-1234567890"))
            .build()
            .unwrap();
        // Reading is any valid token's.
        let read = scoped
            .get(format!("{base}/api/agent-clis"))
            .send()
            .await
            .unwrap();
        assert_eq!(read.status(), StatusCode::OK);
        // Moving the pin is not.
        let write = scoped
            .post(format!("{base}/api/agent-clis/claude-code"))
            .json(&json!({ "version": "2.1.261" }))
            .send()
            .await
            .unwrap();
        assert_eq!(write.status(), StatusCode::FORBIDDEN);
    }
}

/// Poll the live list until `pred` holds for session `id`, returning its row.
async fn wait_for_session_row(
    client: &reqwest::Client,
    base: &str,
    id: &str,
    pred: impl Fn(&Value) -> bool,
) -> Value {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let list: Vec<Value> = client
            .get(format!("{base}/api/sessions"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        if let Some(row) = list.iter().find(|s| s["id"] == id && pred(s)) {
            return row.clone();
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "session {id} never matched; got {list:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn create_session_with(client: &reqwest::Client, base: &str, body: Value) -> String {
    client
        .post(format!("{base}/api/sessions"))
        .json(&body)
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string()
}

/// WI-830: an exited child reads as exited and not alive, and stays that way
/// — the PTY reader's late drain must not flip it back to `running`/`idle`.
#[tokio::test]
async fn an_exited_session_reports_a_terminal_state_and_not_alive() {
    let (base, _h) = boot().await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    let ok = create_session_with(
        &client,
        &base,
        json!({ "name": "true-smoke", "command": ["/bin/sh", "-c", "printf 'bye\\n'; exit 0"] }),
    )
    .await;
    let failing = create_session_with(
        &client,
        &base,
        json!({ "name": "false-smoke", "command": ["/bin/sh", "-c", "printf 'oops\\n'; exit 3"] }),
    )
    .await;
    let live = create_session_with(
        &client,
        &base,
        json!({ "name": "cat", "command": ["/bin/cat"] }),
    )
    .await;

    let row = wait_for_session_row(&client, &base, &ok, |s| s["exit_code"] == json!(0)).await;
    // Well past the 200 ms idle window: nothing may move it off `exited`.
    tokio::time::sleep(Duration::from_millis(600)).await;
    let row_later = wait_for_session_row(&client, &base, &ok, |_| true).await;
    for r in [&row, &row_later] {
        assert_eq!(r["activity"], "exited", "{r:?}");
        assert_eq!(r["alive"], false, "{r:?}");
    }

    let row = wait_for_session_row(&client, &base, &failing, |s| s["exit_code"] == json!(3)).await;
    assert_eq!(row["activity"], "errored", "{row:?}");
    assert_eq!(row["alive"], false, "{row:?}");

    let row = wait_for_session_row(&client, &base, &live, |_| true).await;
    assert_eq!(row["alive"], true, "{row:?}");
    assert!(row["exit_code"].is_null());

    // A stopped (killed) session is not alive either, and reads `stopped`
    // whatever its exit code (WI-913).
    client
        .post(format!("{base}/api/sessions/{live}/kill"))
        .send()
        .await
        .unwrap();
    let row = wait_for_session_row(&client, &base, &live, |s| !s["exit_code"].is_null()).await;
    assert_eq!(row["alive"], false, "{row:?}");
    assert_eq!(row["activity"], "stopped", "{row:?}");

    // History records the exit and why it ended.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let rows: Vec<Value> = client
            .get(format!("{base}/api/history/sessions?limit=20"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        if let Some(r) = rows
            .iter()
            .find(|r| r["id"] == ok && r["exit_code"] == json!(0))
        {
            assert!(r["ended_at"].is_string(), "{r:?}");
            assert_eq!(r["end_reason"], "exited", "{r:?}");
            break;
        }
        assert!(tokio::time::Instant::now() < deadline, "{rows:?}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// WI-831: `/screen` renders the visible grid with the documented shape.
#[tokio::test]
async fn session_screen_renders_the_visible_grid() {
    let (base, _h) = boot().await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    let id = create_session_with(
        &client,
        &base,
        json!({
            "name": "screen",
            "cols": 40,
            "rows": 5,
            "command": [
                "/bin/sh",
                "-c",
                "printf '\\033]0;my-title\\007\\033[2J\\033[1;1HIf\\033[1;4Hnothing\\033[1;12His   \\033[3;1HContinue? [y/N]'; sleep 30",
            ],
        }),
    )
    .await;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let screen = loop {
        let resp = client
            .get(format!("{base}/api/sessions/{id}/screen"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let screen: Value = resp.json().await.unwrap();
        if screen["ready"] == true {
            break screen;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "screen never became ready: {screen:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    };

    let keys: std::collections::BTreeSet<&str> = screen
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        [
            "activity",
            "alive",
            "cols",
            "cursor",
            "id",
            "last_output_at",
            "lines",
            "ready",
            "rows",
            "title",
            "turn_started_at",
        ]
        .into_iter()
        .collect(),
        "{screen:?}"
    );
    assert_eq!(screen["id"], id);
    assert_eq!(screen["cols"], 40);
    assert_eq!(screen["rows"], 5);
    assert_eq!(
        screen["lines"],
        json!(["If nothing is", "", "Continue? [y/N]", "", ""])
    );
    assert_eq!(screen["cursor"], json!({ "row": 2, "col": 15 }));
    assert_eq!(screen["title"], "my-title");
    assert_eq!(screen["activity"], "waiting-for-input");
    assert_eq!(screen["alive"], true);

    // Same gate as the other session reads, and 404 for an unknown id.
    let anon = reqwest::Client::new()
        .get(format!("{base}/api/sessions/{id}/screen"))
        .send()
        .await
        .unwrap();
    assert_eq!(anon.status(), StatusCode::UNAUTHORIZED);
    let missing = client
        .get(format!(
            "{base}/api/sessions/{}/screen",
            uuid::Uuid::new_v4()
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);

    client
        .delete(format!("{base}/api/sessions/{id}"))
        .send()
        .await
        .unwrap();
}

/// WI-877: a permission dialog on screen reads `awaiting-approval`, with the
/// command it asks about (even the part above the screen) and its countdown,
/// on the screen, the list and the event; `ready` is false. WI-875: turn
/// timing is reported. The screen can carry scrollback on request.
#[tokio::test]
async fn a_permission_dialog_reads_awaiting_approval_with_its_command() {
    let (base, _h) = boot().await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    // Ten rows: the dialog's title and the first line of the command scroll
    // off the top, the question and its menu stay on screen.
    let script = "printf 'earlier output\\r\\n────────────\\r\\nBash command\\r\\n\\r\\n  rm -rf /tmp/vogt-approval-test &&\\r\\n  docker rm -f approval-test\\r\\n  clean up\\r\\n\\r\\n'; \
                  for i in 1 2 3 4 5; do printf 'context line %s\\r\\n' $i; done; \
                  printf 'Do you want to proceed?\\r\\n❯ 1. Yes\\r\\n  2. No\\r\\nClaude will automatically deny this request in 90s'; sleep 30";
    let id = create_session_with(
        &client,
        &base,
        json!({
            "name": "approval",
            "cols": 60,
            "rows": 10,
            "command": ["/bin/sh", "-c", script],
        }),
    )
    .await;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let screen = loop {
        let screen: Value = client
            .get(format!(
                "{base}/api/sessions/{id}/screen?scrollback_lines=50"
            ))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        if screen["activity"] == "awaiting-approval" {
            break screen;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "never awaiting-approval: {screen:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert_eq!(
        screen["ready"], false,
        "a dialog is not a prompt to type at"
    );
    let approval = &screen["approval"];
    assert_eq!(approval["question"], "Do you want to proceed?");
    let excerpt = approval["command_excerpt"].as_str().unwrap();
    assert!(
        excerpt.contains("rm -rf /tmp/vogt-approval-test"),
        "the command above the screen is read from the scrollback: {excerpt}"
    );
    assert!(excerpt.contains("docker rm -f approval-test"), "{excerpt}");
    assert!(!excerpt.contains("earlier output"), "{excerpt}");
    let secs = approval["deadline_seconds"].as_u64().unwrap();
    assert!((80..=90).contains(&secs), "{approval:?}");
    assert!(approval["deadline_at"].is_string(), "{approval:?}");
    assert!(screen["turn_started_at"].is_string(), "{screen:?}");
    assert!(screen["last_output_at"].is_string(), "{screen:?}");
    let scrollback = screen["scrollback"].as_array().unwrap();
    assert!(
        scrollback.iter().any(|l| l == "earlier output"),
        "{scrollback:?}"
    );

    // The list says the same.
    let list: Vec<Value> = client
        .get(format!("{base}/api/sessions"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let row = list.iter().find(|s| s["id"] == id).unwrap();
    assert_eq!(row["activity"], "awaiting-approval");
    assert_eq!(row["approval"]["question"], "Do you want to proceed?");

    // An over-large scrollback request is refused, not truncated silently.
    let too_many = client
        .get(format!(
            "{base}/api/sessions/{id}/screen?scrollback_lines=5000"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(too_many.status(), StatusCode::BAD_REQUEST);

    client
        .delete(format!("{base}/api/sessions/{id}"))
        .send()
        .await
        .unwrap();
}

/// WI-874: `/wait` answers as soon as the session is ready, on exit, and on
/// timeout; `any-change` returns on the next state change.
#[tokio::test]
async fn wait_returns_on_ready_exit_change_and_timeout() {
    let (base, _h) = boot().await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let get = |url: String| {
        let client = client.clone();
        async move {
            let resp = client.get(url).send().await.unwrap();
            let status = resp.status();
            (status, resp.json::<Value>().await.unwrap_or(Value::Null))
        }
    };

    // A prompt appears after a moment: the wait blocks, then answers ready.
    let prompt = create_session_with(
        &client,
        &base,
        json!({
            "name": "wait-ready",
            "command": ["/bin/sh", "-c", "sleep 0.5; printf 'Continue? [y/N]'; sleep 30"],
        }),
    )
    .await;
    let (status, ready) = get(format!(
        "{base}/api/sessions/{prompt}/wait?until=ready&timeout_s=20"
    ))
    .await;
    assert_eq!(status, StatusCode::OK, "{ready:?}");
    assert_eq!(ready["outcome"], "ready", "{ready:?}");
    assert_eq!(ready["matched"], true);
    assert_eq!(ready["screen"]["ready"], true);
    assert!(ready["screen"]["lines"][0]
        .as_str()
        .unwrap()
        .contains("Continue?"));

    // Already ready: answers at once.
    let (_, again) = get(format!("{base}/api/sessions/{prompt}/wait?timeout_s=20")).await;
    assert_eq!(again["outcome"], "ready");
    assert!(again["waited_ms"].as_u64().unwrap() < 5000, "{again:?}");

    // Nothing happens: a short timeout says so.
    let (_, timed_out) = get(format!(
        "{base}/api/sessions/{prompt}/wait?until=exited&timeout_s=1"
    ))
    .await;
    assert_eq!(timed_out["outcome"], "timeout");
    assert_eq!(timed_out["matched"], false);

    // A child that exits ends a wait for it.
    let short = create_session_with(
        &client,
        &base,
        json!({ "name": "wait-exit", "command": ["/bin/sh", "-c", "sleep 0.5; exit 3"] }),
    )
    .await;
    let (_, exited) = get(format!(
        "{base}/api/sessions/{short}/wait?until=exited&timeout_s=20"
    ))
    .await;
    assert_eq!(exited["outcome"], "exited", "{exited:?}");
    assert_eq!(exited["matched"], true);
    assert_eq!(exited["screen"]["alive"], false);

    // A blocked report ends a wait for ready (a person must act first), and
    // is on the summary and the screen; clearing it is a change.
    let blocked = client
        .post(format!("{base}/api/sessions/{prompt}/blocked"))
        .json(&json!({ "blocked": true, "reason": "needs the bot token", "items": ["create the bot", " "] }))
        .send()
        .await
        .unwrap();
    assert_eq!(blocked.status(), StatusCode::OK);
    let summary: Value = blocked.json().await.unwrap();
    assert_eq!(summary["blocked"]["reason"], "needs the bot token");
    assert_eq!(summary["blocked"]["items"], json!(["create the bot"]));
    let (_, on_blocked) = get(format!(
        "{base}/api/sessions/{prompt}/wait?until=ready&timeout_s=5"
    ))
    .await;
    assert_eq!(on_blocked["outcome"], "blocked");
    assert_eq!(on_blocked["matched"], false);
    assert_eq!(
        on_blocked["screen"]["blocked"]["reason"],
        "needs the bot token"
    );
    let change = tokio::spawn(get(format!(
        "{base}/api/sessions/{prompt}/wait?until=any-change&timeout_s=20"
    )));
    tokio::time::sleep(Duration::from_millis(300)).await;
    let cleared = client
        .post(format!("{base}/api/sessions/{prompt}/blocked"))
        .json(&json!({ "blocked": false }))
        .send()
        .await
        .unwrap();
    assert_eq!(cleared.status(), StatusCode::OK);
    let cleared: Value = cleared.json().await.unwrap();
    assert!(cleared.get("blocked").is_none(), "{cleared:?}");
    let (_, changed) = change.await.unwrap();
    assert_eq!(changed["outcome"], "changed", "{changed:?}");

    // Bad input is refused.
    let (bad_until, _) = get(format!("{base}/api/sessions/{prompt}/wait?until=soon")).await;
    assert_eq!(bad_until, StatusCode::BAD_REQUEST);
    let (too_long, _) = get(format!("{base}/api/sessions/{prompt}/wait?timeout_s=601")).await;
    assert_eq!(too_long, StatusCode::BAD_REQUEST);
    let no_reason = client
        .post(format!("{base}/api/sessions/{prompt}/blocked"))
        .json(&json!({ "blocked": true }))
        .send()
        .await
        .unwrap();
    assert_eq!(no_reason.status(), StatusCode::BAD_REQUEST);

    for id in [prompt, short] {
        client
            .delete(format!("{base}/api/sessions/{id}"))
            .send()
            .await
            .unwrap();
    }
}

/// WI-830: rows a previous engine process left unfinished (killed by a
/// restart it never drained) are closed out when the next one boots.
#[tokio::test]
async fn startup_closes_out_history_rows_left_unfinished_by_a_restart() {
    use vogt_engine_server::history::{ArchiveRecord, SessionHistory};

    let mut cfg = test_config();
    let state_dir = cfg.state_dir.clone();

    // The previous process: a provisional row and some output, then gone.
    let lost = uuid::Uuid::new_v4();
    {
        let history = SessionHistory::new(&state_dir).await.unwrap();
        history
            .archive_session(ArchiveRecord {
                id: lost,
                name: "lost-to-redeploy".into(),
                created_at: OffsetDateTime::now_utc() - time::Duration::minutes(5),
                ended_at: None,
                exit_code: None,
                cwd: None,
                command: None,
                scrollback_bytes: 0,
                end_reason: None,
                identity: None,
            })
            .await
            .unwrap();
        std::fs::write(history.log_path(lost), b"working...\n").unwrap();
        // An engine from before `end_reason` existed: the next boot must
        // add the column to the existing table.
        sqlx::query("ALTER TABLE sessions DROP COLUMN end_reason")
            .execute(&history.pool)
            .await
            .unwrap();
        history.pool.close().await;
    }

    cfg.state_dir = state_dir.clone();
    let (base, _h) = boot_with_config(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let rows: Vec<Value> = client
        .get(format!("{base}/api/history/sessions?limit=20"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let row = rows.iter().find(|r| r["id"] == lost.to_string()).unwrap();
    assert!(row["ended_at"].is_string(), "{row:?}");
    assert!(row["exit_code"].is_null(), "the code is unknown: {row:?}");
    assert_eq!(row["end_reason"], "engine-restart", "{row:?}");

    // A session of this process is not touched by the reconcile. Its
    // provisional history row is written by a task spawned at launch, so it
    // is polled for rather than assumed to have landed within a fixed sleep:
    // on a loaded runner the spawn had not run 200 ms later and the row was
    // simply absent.
    let id = create_session_with(
        &client,
        &base,
        json!({ "name": "fresh", "command": ["/bin/cat"] }),
    )
    .await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let row = loop {
        let rows: Vec<Value> = client
            .get(format!("{base}/api/history/sessions?limit=20"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        if let Some(row) = rows.iter().find(|r| r["id"] == id) {
            break row.clone();
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the fresh session never got its provisional history row: {rows:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert!(row["ended_at"].is_null(), "{row:?}");
    assert!(row["end_reason"].is_null(), "{row:?}");

    // A live child holds a blocking-pool waiter the test runtime would wait
    // on forever at shutdown.
    client
        .delete(format!("{base}/api/sessions/{id}"))
        .send()
        .await
        .unwrap();
}

// ---------------------------------------------------------------------------
// Hibernation (WI-912)
// ---------------------------------------------------------------------------

/// Poll a live session's scrollback until it holds every `expected` string.
async fn live_output_containing(
    client: &reqwest::Client,
    base: &str,
    id: &str,
    expected: &[&str],
) -> String {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        let detail: SessionDetail = client
            .get(format!("{base}/api/sessions/{id}"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let snapshot = base64::engine::general_purpose::STANDARD
            .decode(detail.scrollback_base64.as_bytes())
            .unwrap();
        let printed = String::from_utf8_lossy(&snapshot).into_owned();
        if expected.iter().all(|want| printed.contains(want)) {
            return printed;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "session {id} never printed {expected:?}; got {printed:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// A sandbox whose `bin/claude` stands in for Claude Code: it prints its
/// arguments and two variables, starts a long-lived child of its own (an MCP
/// server's stand-in) and reports its pid, then waits at a prompt.
fn hibernation_sandbox() -> (tempfile::TempDir, Config, std::path::PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = test_config();
    cfg.default_cwd = tmp.path().to_path_buf();
    cfg.workspace_root = tmp.path().canonicalize().unwrap();
    cfg.state_dir = tmp.path().join("state");
    let bin = tmp.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let stub = bin.join("claude");
    std::fs::write(
        &stub,
        "#!/bin/sh\nfor a in \"$@\"; do printf 'arg=[%s]\\n' \"$a\"; done\n\
         printf 'token=[%s] keep=[%s]\\n' \"$VOGT_HTTP_TOKEN\" \"$KEEP\"\n\
         sleep 300 &\nprintf 'helper=[%s]\\n' \"$!\"\nprintf '> '\nwait\n",
    )
    .unwrap();
    std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
    (tmp, cfg, stub)
}

fn pid_alive(pid: i32) -> bool {
    // A zombie still answers kill(0); count it as gone.
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
    match stat.rfind(')') {
        Some(at) => !stat[at + 2..].starts_with('Z'),
        None => false,
    }
}

fn printed_value(printed: &str, key: &str) -> String {
    let start = printed.rfind(&format!("{key}=[")).unwrap() + key.len() + 2;
    let end = start + printed[start..].find(']').unwrap();
    printed[start..end].to_string()
}

#[tokio::test]
async fn a_hibernated_agent_frees_its_processes_and_wakes_into_the_same_conversation() {
    let (tmp, cfg, stub) = hibernation_sandbox();
    let state_dir = cfg.state_dir.clone();
    let (base, _h) = boot_with_config(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();

    let created: Value = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({
            "name": "sleepy agent",
            "prompt": "## Task\n\nDo the thing.\n",
            "command": [stub.to_string_lossy()],
            "env": [["VOGT_HTTP_TOKEN", "first-secret"], ["KEEP", "kept"]],
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = created["id"].as_str().unwrap().to_string();
    assert_eq!(created["conversation"]["agent"], "claude");
    assert_eq!(created["conversation"]["id"], id.as_str());
    let printed = live_output_containing(&client, &base, &id, &["helper=[", "> "]).await;
    assert!(
        printed.contains(&format!("arg=[--session-id]\r\narg=[{id}]")),
        "{printed:?}"
    );
    let helper: i32 = printed_value(&printed, "helper").parse().unwrap();
    assert!(pid_alive(helper));

    // The record exists from the start, and holds no secret.
    let record_path = state_dir.join("sessions").join(format!("{id}.json"));
    let record = std::fs::read_to_string(&record_path).unwrap();
    assert!(!record.contains("first-secret"), "{record}");
    assert!(record.contains("\"KEEP\""), "{record}");

    // Hibernate: the whole tree goes, the session stays listed.
    let hibernated = client
        .post(format!("{base}/api/sessions/{id}/hibernate"))
        .json(&json!({ "reason": "idle for a test" }))
        .send()
        .await
        .unwrap();
    assert_eq!(hibernated.status(), StatusCode::OK);
    let hibernated: Value = hibernated.json().await.unwrap();
    assert_eq!(hibernated["activity"], "hibernated");
    assert_eq!(hibernated["alive"], false);
    assert_eq!(hibernated["hibernation"]["trigger"], "manual");
    assert_eq!(hibernated["hibernation"]["reason"], "idle for a test");
    assert_eq!(hibernated["hibernation"]["resumable"], true);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(!pid_alive(helper), "the agent's child should be gone");

    let listed: Vec<Value> = client
        .get(format!("{base}/api/sessions"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let row = listed.iter().find(|s| s["id"] == id.as_str()).unwrap();
    assert_eq!(row["activity"], "hibernated");

    // Its last screen is still readable, and reading it does not wake it.
    let screen: Value = client
        .get(format!("{base}/api/sessions/{id}/screen"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(screen["activity"], "hibernated");
    assert_eq!(screen["ready"], false);
    let lines = screen["lines"].as_array().unwrap();
    assert!(
        lines
            .iter()
            .any(|l| l.as_str().unwrap().contains("keep=[kept]")),
        "{screen}"
    );

    // Input is refused with the way to wake it.
    let typed = client
        .post(format!("{base}/api/sessions/{id}/input"))
        .json(&json!({ "text": "hello" }))
        .send()
        .await
        .unwrap();
    assert_eq!(typed.status(), StatusCode::CONFLICT);
    assert!(typed.text().await.unwrap().contains("wake"));

    // Wake under the same id, resuming the conversation, with a new token,
    // without being told to read the brief again.
    let woken = client
        .post(format!("{base}/api/sessions/{id}/wake"))
        .json(&json!({ "env": [["VOGT_HTTP_TOKEN", "second-secret"]] }))
        .send()
        .await
        .unwrap();
    assert_eq!(woken.status(), StatusCode::OK);
    let woken: Value = woken.json().await.unwrap();
    assert_eq!(woken["id"], id.as_str());
    assert_eq!(woken["alive"], true);
    let printed = live_output_containing(&client, &base, &id, &["helper=["]).await;
    assert!(
        printed.contains(&format!("arg=[--resume]\r\narg=[{id}]")),
        "{printed:?}"
    );
    assert!(!printed.contains("--session-id"), "{printed:?}");
    assert!(
        !printed.contains("Vogt started this session"),
        "{printed:?}"
    );
    assert!(
        printed.contains("token=[second-secret] keep=[kept]"),
        "{printed:?}"
    );
    let record = std::fs::read_to_string(&record_path).unwrap();
    assert!(
        !record.contains("second-secret") && !record.contains("\"hibernation\""),
        "{record}"
    );

    // Killing it ends it: the record goes, so a restart will not bring it back.
    let killed = client
        .post(format!("{base}/api/sessions/{id}/kill"))
        .send()
        .await
        .unwrap();
    assert_eq!(killed.status(), StatusCode::OK);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while record_path.exists() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the record outlived the kill"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    drop(tmp);
}

#[tokio::test]
async fn a_shell_hibernates_only_when_asked_and_wakes_fresh() {
    let (_tmp, cfg, _stub) = hibernation_sandbox();
    let (base, _h) = boot_with_config(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let created: Value = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({ "name": "plain shell" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = created["id"].as_str().unwrap().to_string();
    assert!(created.get("conversation").is_none());

    let refused = client
        .post(format!("{base}/api/sessions/{id}/hibernate"))
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), StatusCode::CONFLICT);
    assert!(refused.text().await.unwrap().contains("allow_shell"));

    let hibernated: Value = client
        .post(format!("{base}/api/sessions/{id}/hibernate"))
        .json(&json!({ "allow_shell": true }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(hibernated["hibernation"]["resumable"], false);

    let woken: Value = client
        .post(format!("{base}/api/sessions/{id}/wake"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(woken["alive"], true);
    assert_eq!(woken["id"], id.as_str());
}

#[tokio::test]
async fn sessions_survive_an_engine_restart_as_hibernated() {
    let (_tmp, cfg, stub) = hibernation_sandbox();
    let state_dir = cfg.state_dir.clone();

    // A graceful shutdown hibernates every agent session it can.
    let (base, state, guard) = boot_with_state(cfg.clone()).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let created: Value = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({ "name": "survivor", "command": [stub.to_string_lossy()] }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = created["id"].as_str().unwrap().to_string();
    live_output_containing(&client, &base, &id, &["helper=["]).await;
    state.sessions.hibernate_for_shutdown().await;
    drop(guard);

    // An engine that died without hibernating leaves its write-ahead record:
    // stand one in for a session the engine never got to.
    let lost = uuid::Uuid::new_v4();
    std::fs::write(
        state_dir.join("sessions").join(format!("{lost}.json")),
        serde_json::to_vec(&json!({
            "version": 1,
            "id": lost,
            "name": "lost in a SIGKILL",
            "created_at": "2026-10-05T00:00:00Z",
            "command": [stub.to_string_lossy()],
            "conversation": { "agent": "claude", "id": lost },
        }))
        .unwrap(),
    )
    .unwrap();

    let (base, _h) = boot_with_config(cfg).await;
    let listed: Vec<Value> = client
        .get(format!("{base}/api/sessions"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let find = |want: &str| {
        listed
            .iter()
            .find(|s| s["id"] == want)
            .unwrap_or_else(|| panic!("{want} not listed in {listed:?}"))
            .clone()
    };
    let survivor = find(&id);
    assert_eq!(survivor["activity"], "hibernated");
    assert_eq!(survivor["hibernation"]["trigger"], "shutdown");
    let recovered = find(&lost.to_string());
    assert_eq!(recovered["hibernation"]["trigger"], "recovered");

    let woken: Value = client
        .post(format!("{base}/api/sessions/{id}/wake"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(woken["alive"], true);
    let printed = live_output_containing(&client, &base, &id, &["helper=["]).await;
    assert!(
        printed.contains(&format!("arg=[--resume]\r\narg=[{id}]")),
        "{printed:?}"
    );
}

#[tokio::test]
async fn attaching_to_a_hibernated_session_replays_its_screen_and_does_not_wake_it() {
    let (_tmp, cfg, stub) = hibernation_sandbox();
    let (base, _h) = boot_with_config(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let created: Value = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({ "name": "attach me", "command": [stub.to_string_lossy()] }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = created["id"].as_str().unwrap().to_string();
    live_output_containing(&client, &base, &id, &["helper=["]).await;
    let status = client
        .post(format!("{base}/api/sessions/{id}/hibernate"))
        .send()
        .await
        .unwrap()
        .status();
    assert_eq!(status, StatusCode::OK);

    let ws_url = format!(
        "{}/api/sessions/{id}/attach",
        base.replacen("http://", "ws://", 1)
    );
    let (mut ws, _) = tokio_tungstenite::connect_async(ws_url).await.unwrap();
    ws.send(Message::Text(
        json!({ "type": "auth", "token": TEST_TOKEN })
            .to_string()
            .into(),
    ))
    .await
    .unwrap();
    let mut texts = Vec::new();
    let mut bytes = Vec::new();
    while let Some(Ok(msg)) = ws.next().await {
        match msg {
            Message::Text(t) => texts.push(t.to_string()),
            Message::Binary(b) => bytes.extend_from_slice(&b),
            Message::Close(_) => break,
            _ => {}
        }
    }
    assert!(
        texts.iter().any(|t| t.contains("snapshot-start")),
        "{texts:?}"
    );
    assert!(
        texts.last().unwrap().contains("\"hibernated\""),
        "{texts:?}"
    );
    assert!(String::from_utf8_lossy(&bytes).contains("helper=["));

    let listed: Vec<Value> = client
        .get(format!("{base}/api/sessions"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let row = listed.iter().find(|s| s["id"] == id.as_str()).unwrap();
    assert_eq!(row["activity"], "hibernated", "attach must not wake it");
}

/// Start one stub agent per `(name, script)` in the hibernation sandbox and
/// return their ids once each has printed `helper=[`.
async fn start_stub_agents(
    client: &reqwest::Client,
    base: &str,
    bin: &std::path::Path,
    agents: &[(&str, &str)],
) -> Vec<String> {
    let mut ids = Vec::new();
    for (name, script) in agents {
        // Each stub is named `claude` (that is how the engine tells an agent
        // CLI), in a directory of its own.
        let dir = bin.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        let stub = dir.join("claude");
        std::fs::write(&stub, script).unwrap();
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
        let created: Value = client
            .post(format!("{base}/api/sessions"))
            .json(&json!({ "name": name, "command": [stub.to_string_lossy()] }))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        ids.push(created["id"].as_str().unwrap().to_string());
    }
    for id in &ids {
        live_output_containing(client, base, id, &["helper=["]).await;
    }
    ids
}

#[tokio::test]
async fn the_idle_policy_hibernates_only_what_no_exemption_covers() {
    use vogt_engine_server::hibernate_policy::{exemption, run_once, Policy};

    let (tmp, cfg, _stub) = hibernation_sandbox();
    let (base, state, _h) = boot_with_state(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let quiet = "#!/bin/sh\nsleep 300 &\nprintf 'helper=[%s]\\n> ' \"$!\"\nwait\n";
    // A tool call in progress: a shell below the agent.
    let tooling = "#!/bin/sh\nsh -c 'sleep 300' &\nprintf 'helper=[%s]\\n> ' \"$!\"\nwait\n";
    // A turn that keeps printing.
    let busy = "#!/bin/sh\nprintf 'helper=[0]\\n'\nwhile :; do printf .; sleep 0.02; done\n";
    let ids = start_stub_agents(
        &client,
        &base,
        &tmp.path().join("agents"),
        &[
            ("quiet", quiet),
            ("pinned", quiet),
            ("blocked", quiet),
            ("tooling", tooling),
            ("busy", busy),
        ],
    )
    .await;
    let [quiet_id, pinned, blocked, tooling_id, busy_id] = [0, 1, 2, 3, 4].map(|i| ids[i].clone());
    let ok = client
        .post(format!("{base}/api/sessions/{pinned}/keep-awake"))
        .json(&json!({ "keep_awake": true }))
        .send()
        .await
        .unwrap()
        .status();
    assert_eq!(ok, StatusCode::OK);
    let ok = client
        .post(format!("{base}/api/sessions/{blocked}/blocked"))
        .json(&json!({ "blocked": true, "reason": "needs the operator" }))
        .send()
        .await
        .unwrap()
        .status();
    assert_eq!(ok, StatusCode::OK);
    let why = |id: &str| {
        let session = state.sessions.get(id.parse().unwrap()).unwrap();
        exemption(&state.sessions, &session)
    };
    // Until the quiet ones settle at idle (200 ms here), the quiet one has
    // been quiet past the policy's 500 ms, and the busy one is seen running; a loaded runner can take longer than any fixed sleep.
    let settled = || {
        why(&quiet_id).is_none()
            && state
                .sessions
                .get(quiet_id.parse().unwrap())
                .unwrap()
                .quiet_for()
                >= Duration::from_millis(700)
            && why(&pinned).as_deref() == Some("pinned awake")
            && why(&blocked).as_deref() == Some("blocked on a person")
            && why(&tooling_id).is_some_and(|w| w.contains("shell is running below the agent"))
            && why(&busy_id).as_deref() == Some("a turn is running")
    };
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !settled() && std::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(why(&quiet_id), None);
    assert_eq!(why(&pinned).as_deref(), Some("pinned awake"));
    assert_eq!(why(&blocked).as_deref(), Some("blocked on a person"));
    assert!(
        why(&tooling_id).is_some_and(|w| w.contains("shell is running below the agent")),
        "{:?}",
        why(&tooling_id)
    );
    assert_eq!(why(&busy_id).as_deref(), Some("a turn is running"));

    run_once(
        &state.sessions,
        &Policy {
            idle_after: Some(Duration::from_millis(500)),
            memavailable_below: None,
        },
    )
    .await;

    let listed: Vec<Value> = client
        .get(format!("{base}/api/sessions"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let activity = |id: &str| {
        listed
            .iter()
            .find(|s| s["id"] == id)
            .map(|s| s["activity"].as_str().unwrap().to_string())
            .unwrap()
    };
    assert_eq!(activity(&quiet_id), "hibernated");
    let row = listed
        .iter()
        .find(|s| s["id"] == quiet_id.as_str())
        .unwrap();
    assert_eq!(row["hibernation"]["trigger"], "idle");
    for id in [&pinned, &blocked, &tooling_id, &busy_id] {
        assert_ne!(activity(id), "hibernated", "{id} should have been exempt");
    }
}

#[tokio::test]
async fn a_session_pinned_awake_wakes_by_itself_after_a_restart() {
    let (_tmp, cfg, stub) = hibernation_sandbox();
    let (base, state, guard) = boot_with_state(cfg.clone()).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let mut ids = Vec::new();
    for name in ["the driver", "a worker"] {
        let created: Value = client
            .post(format!("{base}/api/sessions"))
            .json(&json!({ "name": name, "command": [stub.to_string_lossy()] }))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        ids.push(created["id"].as_str().unwrap().to_string());
    }
    for id in &ids {
        live_output_containing(&client, &base, id, &["helper=["]).await;
    }
    client
        .post(format!("{base}/api/sessions/{}/keep-awake", ids[0]))
        .json(&json!({ "keep_awake": true }))
        .send()
        .await
        .unwrap();
    state.sessions.hibernate_for_shutdown().await;
    drop(guard);

    let (base, _h) = boot_with_config(cfg).await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        let listed: Vec<Value> = client
            .get(format!("{base}/api/sessions"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let activity = |id: &str| {
            listed
                .iter()
                .find(|s| s["id"] == id)
                .map(|s| s["activity"].as_str().unwrap().to_string())
        };
        if activity(&ids[0]).as_deref() != Some("hibernated") {
            assert_eq!(
                activity(&ids[1]).as_deref(),
                Some("hibernated"),
                "only the pinned one wakes"
            );
            let driver = listed.iter().find(|s| s["id"] == ids[0].as_str()).unwrap();
            assert_eq!(driver["alive"], true);
            assert_eq!(driver["keep_awake"], true, "the pin survives the wake");
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the pinned session never woke"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test]
async fn an_oversight_session_is_pinned_listed_as_oversight_and_back_after_a_restart() {
    let (_tmp, cfg, stub) = hibernation_sandbox();
    let (base, state, guard) = boot_with_state(cfg.clone()).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    // One nominated at start, one nominated afterwards, one left a worker.
    let mut ids = Vec::new();
    for (name, role) in [
        ("overseer", Some("oversight")),
        ("promoted", None),
        ("a worker", None),
    ] {
        let mut body = json!({ "name": name, "command": [stub.to_string_lossy()] });
        if let Some(role) = role {
            body["role"] = json!(role);
        }
        let created: Value = client
            .post(format!("{base}/api/sessions"))
            .json(&body)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        ids.push(created["id"].as_str().unwrap().to_string());
    }
    for id in &ids {
        live_output_containing(&client, &base, id, &["helper=["]).await;
    }
    let promoted: Value = client
        .post(format!("{base}/api/sessions/{}/role", ids[1]))
        .json(&json!({ "role": "oversight" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(promoted["role"], "oversight");
    assert_eq!(
        promoted["keep_awake"], true,
        "oversight pins the session awake"
    );

    let listed: Vec<Value> = client
        .get(format!("{base}/api/sessions"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let row = |id: &str| listed.iter().find(|s| s["id"] == id).unwrap().clone();
    assert_eq!(row(&ids[0])["role"], "oversight");
    assert_eq!(row(&ids[0])["keep_awake"], true);
    assert!(
        row(&ids[2]).get("role").is_none(),
        "a worker carries no role on the wire"
    );
    assert!(row(&ids[2]).get("keep_awake").is_none());

    // Unknown roles are refused rather than read as a worker.
    let refused = client
        .post(format!("{base}/api/sessions/{}/role", ids[2]))
        .json(&json!({ "role": "boss" }))
        .send()
        .await
        .unwrap();
    assert!(refused.status().is_client_error());

    state.sessions.hibernate_for_shutdown().await;
    drop(guard);

    let (base, _h) = boot_with_config(cfg).await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        let listed: Vec<Value> = client
            .get(format!("{base}/api/sessions"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let row = |id: &str| listed.iter().find(|s| s["id"] == id).cloned();
        let awake = |id: &str| row(id).is_some_and(|r| r["activity"] != "hibernated");
        if awake(&ids[0]) && awake(&ids[1]) {
            for id in &ids[..2] {
                let r = row(id).unwrap();
                assert_eq!(r["role"], "oversight", "the role survives the restart");
                assert_eq!(r["alive"], true);
            }
            assert_eq!(
                row(&ids[2]).unwrap()["activity"],
                "hibernated",
                "the worker waits to be woken"
            );
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the oversight sessions never woke"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    // A woken overseer is told, at its prompt, that it was resumed (WI-962).
    for id in &ids[..2] {
        live_output_containing(
            &client,
            &base,
            id,
            &["[vogt] This oversight session was resumed after the engine restarted"],
        )
        .await;
    }
}

#[tokio::test]
async fn a_work_item_label_is_set_cleared_kept_in_history_and_survives_a_restart() {
    let (_tmp, cfg, stub) = hibernation_sandbox();
    let (base, state, guard) = boot_with_state(cfg.clone()).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    // One labelled at start, one labelled afterwards.
    let mut ids = Vec::new();
    for (name, work_item) in [("on WI-7", Some(" WI-7 ")), ("bound later", None)] {
        let mut body = json!({ "name": name, "command": [stub.to_string_lossy()] });
        if let Some(work_item) = work_item {
            body["work_item"] = json!(work_item);
        }
        let created: Value = client
            .post(format!("{base}/api/sessions"))
            .json(&body)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        ids.push(created["id"].as_str().unwrap().to_string());
    }
    for id in &ids {
        live_output_containing(&client, &base, id, &["helper=["]).await;
    }
    let set = |id: String, work_item: Value| {
        let client = client.clone();
        let base = base.clone();
        async move {
            client
                .post(format!("{base}/api/sessions/{id}/work-item"))
                .json(&json!({ "work_item": work_item }))
                .send()
                .await
                .unwrap()
        }
    };
    let bound: Value = set(ids[1].clone(), json!("WI-8"))
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(bound["work_item"], "WI-8");

    // A label the engine will not keep is refused, not cut; the old one stays.
    let refused = set(ids[1].clone(), json!("x".repeat(201))).await;
    assert_eq!(refused.status(), reqwest::StatusCode::BAD_REQUEST);
    let missing = set(uuid::Uuid::new_v4().to_string(), json!("WI-1")).await;
    assert_eq!(missing.status(), reqwest::StatusCode::NOT_FOUND);

    let listed: Vec<Value> = client
        .get(format!("{base}/api/sessions"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let row = |id: &str| listed.iter().find(|s| s["id"] == id).unwrap().clone();
    assert_eq!(row(&ids[0])["work_item"], "WI-7", "trimmed at start");
    assert_eq!(row(&ids[1])["work_item"], "WI-8");

    // History knows it while the session runs; an unbind clears it there too.
    let cleared: Value = set(ids[0].clone(), Value::Null).await.json().await.unwrap();
    assert!(
        cleared.get("work_item").is_none(),
        "unbound carries no label"
    );
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let first: Value = client
            .get(format!("{base}/api/history/{}", ids[0]))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let second: Value = client
            .get(format!("{base}/api/history/{}", ids[1]))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        if second["work_item"] == "WI-8" && first.get("work_item").is_none() {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "history never showed the work items: {first} {second}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // The record keeps it through a restart.
    state.sessions.hibernate_for_shutdown().await;
    drop(guard);
    let (base, _h) = boot_with_config(cfg).await;
    let listed: Vec<Value> = client
        .get(format!("{base}/api/sessions"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let kept = listed.iter().find(|s| s["id"] == ids[1].as_str()).unwrap();
    assert_eq!(kept["activity"], "hibernated");
    assert_eq!(kept["work_item"], "WI-8", "the label survives the restart");
}

/// [`hibernation_sandbox`] with a `claude` template that runs the stub, the
/// way a deployment's templates run Claude Code: what a wake resumes a
/// reported conversation through (WI-962).
fn conversation_sandbox() -> (tempfile::TempDir, Config, std::path::PathBuf) {
    let (tmp, mut cfg, stub) = hibernation_sandbox();
    cfg.session_templates.push(SessionTemplate {
        name: "Claude Code (test)".to_string(),
        description: "the stand-in claude".to_string(),
        command: Some(vec![stub.to_string_lossy().into_owned()]),
        cwd: None,
        env: vec![],
        default_name: None,
        match_repo_names: vec![],
        match_path_prefixes: vec![],
        tags: vec!["claude".to_string()],
    });
    (tmp, cfg, stub)
}

/// Stand in for a SIGKILL of the engine: end the session (so this engine's
/// exit hook has run and cannot race the next engine) and put back the
/// write-ahead record a killed engine would have left.
async fn lose_session(
    client: &reqwest::Client,
    base: &str,
    id: &str,
    record_path: &std::path::Path,
) {
    let record = std::fs::read_to_string(record_path).unwrap();
    client
        .post(format!("{base}/api/sessions/{id}/kill"))
        .send()
        .await
        .unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while record_path.exists() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the record outlived the kill"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    std::fs::write(record_path, record).unwrap();
}

async fn session_row(client: &reqwest::Client, base: &str, id: &str) -> Value {
    let listed: Vec<Value> = client
        .get(format!("{base}/api/sessions"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    listed
        .into_iter()
        .find(|s| s["id"] == id)
        .unwrap_or_else(|| panic!("{id} is not listed"))
}

/// The 2026-10-07 incident (WI-962): a `claude` typed by hand into a plain
/// shell. The engine saw a shell, the redeploy's SIGKILL left a record with
/// no conversation, boot dropped it, and History showed only `bash`. Now the
/// agent reports its conversation from inside the session, the record keeps
/// it, the session comes back hibernated and resumable, a wake starts the
/// agent on that conversation, and History shows and offers it.
#[tokio::test]
async fn a_claude_typed_into_a_shell_is_linked_kept_across_a_sigkill_and_resumed() {
    let (_tmp, cfg, _stub) = conversation_sandbox();
    let state_dir = cfg.state_dir.clone();
    let (base, guard) = boot_with_config(cfg.clone()).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let created: Value = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({ "name": "Oversight" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = created["id"].as_str().unwrap().to_string();
    assert!(
        created.get("conversation").is_none(),
        "a shell, to start with"
    );

    let conversation = "6c1f0d2e-5b7a-4e1c-9f3d-2a8b7c6d5e4f";
    for bad in [
        json!({ "agent": "bash", "id": conversation }),
        json!({ "agent": "claude", "id": "../../etc/passwd" }),
    ] {
        let refused = client
            .post(format!("{base}/api/sessions/{id}/conversation"))
            .json(&bad)
            .send()
            .await
            .unwrap();
        assert_eq!(refused.status(), StatusCode::BAD_REQUEST, "{bad}");
    }
    let linked: Value = client
        .post(format!("{base}/api/sessions/{id}/conversation"))
        .json(&json!({ "agent": "claude", "id": conversation }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(linked["conversation"]["agent"], "claude");
    assert_eq!(linked["conversation"]["id"], conversation);

    // What a SIGKILL leaves behind is the write-ahead record: it now holds
    // the conversation.
    let record_path = state_dir.join("sessions").join(format!("{id}.json"));
    let record = std::fs::read_to_string(&record_path).unwrap();
    assert!(record.contains(conversation), "{record}");
    drop(record);

    // History knows what the session is, without waiting for it to end.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let row: Value = client
            .get(format!("{base}/api/history/{id}"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        if row["conversation_id"] == conversation {
            assert_eq!(row["conversation_agent"], "claude");
            assert_eq!(row["role"], "worker");
            assert_eq!(row["resume_template"], "claude");
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "history never showed the conversation: {row}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // The engine dies without hibernating anything.
    lose_session(&client, &base, &id, &record_path).await;
    drop(guard);

    let (base, _h) = boot_with_config(cfg).await;
    let recovered = session_row(&client, &base, &id).await;
    assert_eq!(recovered["activity"], "hibernated", "{recovered}");
    assert_eq!(recovered["hibernation"]["trigger"], "recovered");
    assert_eq!(recovered["hibernation"]["resumable"], true);
    assert_eq!(recovered["conversation"]["id"], conversation);

    // Waking it starts the agent on its conversation, not an empty shell.
    let woken = client
        .post(format!("{base}/api/sessions/{id}/wake"))
        .send()
        .await
        .unwrap();
    assert_eq!(woken.status(), StatusCode::OK);
    let printed = live_output_containing(&client, &base, &id, &["helper=["]).await;
    assert!(
        printed.contains(&format!("arg=[--resume]\r\narg=[{conversation}]")),
        "{printed:?}"
    );

    // History still offers the resume after a restart.
    let rows: Vec<Value> = client
        .get(format!("{base}/api/history/sessions"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let row = rows.iter().find(|r| r["id"] == id.as_str()).unwrap();
    assert_eq!(row["conversation_id"], conversation);
    assert_eq!(row["resume_template"], "claude");
}

/// A shell nobody reported a conversation from is still forgotten at boot —
/// unless it is the overseer, which stays listed, not resumable, and is not
/// woken into an empty shell (WI-962).
#[tokio::test]
async fn an_oversight_shell_stays_listed_after_a_sigkill_and_a_worker_shell_does_not() {
    let (_tmp, cfg, _stub) = conversation_sandbox();
    let state_dir = cfg.state_dir.clone();
    let (base, guard) = boot_with_config(cfg.clone()).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let mut records = Vec::new();
    for (name, role) in [("the overseer", "oversight"), ("a shell", "worker")] {
        let created: Value = client
            .post(format!("{base}/api/sessions"))
            .json(&json!({ "name": name, "role": role }))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let id = created["id"].as_str().unwrap().to_string();
        let path = state_dir.join("sessions").join(format!("{id}.json"));
        lose_session(&client, &base, &id, &path).await;
        records.push((id, path));
    }
    drop(guard);

    let (base, _h) = boot_with_config(cfg).await;
    // Give a boot wake time to (wrongly) happen.
    tokio::time::sleep(Duration::from_millis(500)).await;
    let overseer = session_row(&client, &base, &records[0].0).await;
    assert_eq!(overseer["activity"], "hibernated", "{overseer}");
    assert_eq!(overseer["role"], "oversight");
    assert_eq!(overseer["hibernation"]["resumable"], false);
    let listed: Vec<Value> = client
        .get(format!("{base}/api/sessions"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        !listed.iter().any(|s| s["id"] == records[1].0.as_str()),
        "a lost worker shell is forgotten: {listed:?}"
    );
    assert!(!records[1].1.exists());
}

/// A graceful shutdown keeps an oversight shell too, with its screen, rather
/// than leaving it to the history drain (WI-962). It is not woken at boot.
#[tokio::test]
async fn a_shutdown_hibernates_an_oversight_shell_and_boot_leaves_it_hibernated() {
    let (_tmp, cfg, _stub) = conversation_sandbox();
    let (base, state, guard) = boot_with_state(cfg.clone()).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let created: Value = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({
            "name": "overseer shell",
            "role": "oversight",
            "command": ["/bin/bash", "-c", "echo overseeing; sleep 300"],
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = created["id"].as_str().unwrap().to_string();
    live_output_containing(&client, &base, &id, &["overseeing"]).await;
    state.sessions.hibernate_for_shutdown().await;
    drop(guard);

    let (base, _h) = boot_with_config(cfg).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let row = session_row(&client, &base, &id).await;
    assert_eq!(row["activity"], "hibernated", "{row}");
    assert_eq!(row["hibernation"]["trigger"], "shutdown");
    assert_eq!(row["hibernation"]["resumable"], false);
    assert_eq!(row["role"], "oversight");
}

/// `vogt-claude-session-hook install`, which the entrypoint runs: both hooks
/// land in Claude Code's user settings once, beside what was there, and a
/// file that is not JSON is left alone.
#[test]
fn the_claude_session_hook_installs_itself_once_and_keeps_the_settings() {
    if std::process::Command::new("python3")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("skipping: install is written in python3");
        return;
    }
    let hook = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../deploy/claude-session-hook.sh")
        .canonicalize()
        .unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let settings = tmp.path().join("settings.json");
    std::fs::write(
        &settings,
        r#"{"theme":"dark","hooks":{"Stop":[{"hooks":[{"type":"command","command":"x"}]}]}}"#,
    )
    .unwrap();
    for _ in 0..2 {
        let status = std::process::Command::new(&hook)
            .arg("install")
            .arg(&settings)
            .status()
            .unwrap();
        assert!(status.success());
    }
    let written: Value =
        serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
    assert_eq!(written["theme"], "dark");
    assert_eq!(written["hooks"]["Stop"][0]["hooks"][0]["command"], "x");
    for (event, arg) in [("SessionStart", "start"), ("SessionEnd", "end")] {
        let groups = written["hooks"][event].as_array().unwrap();
        assert_eq!(groups.len(), 1, "{event} added once: {written}");
        assert_eq!(
            groups[0]["hooks"][0]["command"],
            format!("{} {arg}", hook.display())
        );
    }
    let broken = tmp.path().join("broken.json");
    std::fs::write(&broken, "not json").unwrap();
    let status = std::process::Command::new(&hook)
        .arg("install")
        .arg(&broken)
        .status()
        .unwrap();
    assert!(status.success());
    assert_eq!(std::fs::read_to_string(&broken).unwrap(), "not json");
}

/// The hook itself (`engine/deploy/claude-session-hook.sh`), as Claude Code
/// runs it inside a session: it links the conversation through the
/// session's own broker token, unlinks it on `end` only while it is still
/// the session's, and ignores a `claude` whose stdin is not a terminal.
#[tokio::test]
async fn the_claude_session_hook_links_and_unlinks_the_conversation_it_runs_in() {
    let hook = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../deploy/claude-session-hook.sh")
        .canonicalize()
        .unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let cfg = broker_config(&tmp, "LATER proj later ondemand\n");
    // A stand-in `claude` that runs the hook the way Claude Code does, with
    // the hook's JSON on stdin.
    let cli = tmp.path().join("claude");
    std::fs::write(
        &cli,
        format!(
            "#!/bin/bash\nprintf '{{\"session_id\":\"%s\",\"hook_event_name\":\"x\"}}' \"$2\" | '{}' \"$1\"\n",
            hook.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o755)).unwrap();
    let (base, _h) = boot_with_config(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let first = "11111111-2222-4333-8444-555555555555";
    let second = "66666666-7777-4888-9999-aaaaaaaaaaaa";
    let other = "bbbbbbbb-cccc-4ddd-8eee-ffffffffffff";
    let cli = cli.display();
    let script = format!(
        "'{cli}' start {first} </dev/null; echo step-1; read; \
         '{cli}' start {first}; echo step-2; read; \
         '{cli}' end {other}; echo step-3; read; \
         '{cli}' start {second}; '{cli}' end {first}; echo step-4; read; \
         '{cli}' end {second}; echo step-5; sleep 30"
    );
    let id = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({
            "name": "hand-typed claude",
            "command": ["/bin/bash", "-c", script],
            "env": [["VOGT_ENGINE_BROKER_URL", base]],
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
    let conversation = |row: &Value| row["conversation"]["id"].as_str().map(str::to_string);
    let step = |n: u32| {
        let client = client.clone();
        let base = base.clone();
        let id = id.clone();
        async move {
            live_output_containing(&client, &base, &id, &[&format!("step-{n}")]).await;
            session_row(&client, &base, &id).await
        }
    };
    let next = |client: &reqwest::Client| {
        client
            .post(format!("{base}/api/sessions/{id}/input"))
            .json(&json!({ "text": "\r" }))
            .send()
    };

    // A `claude` with no terminal on stdin is not the session's own.
    assert_eq!(conversation(&step(1).await), None);
    next(&client).await.unwrap();
    assert_eq!(conversation(&step(2).await).as_deref(), Some(first));
    next(&client).await.unwrap();
    // The end of a conversation that is not the session's changes nothing.
    assert_eq!(conversation(&step(3).await).as_deref(), Some(first));
    next(&client).await.unwrap();
    // `/clear`: the new conversation is reported, then the old one ends.
    assert_eq!(conversation(&step(4).await).as_deref(), Some(second));
    next(&client).await.unwrap();
    assert_eq!(conversation(&step(5).await), None);

    // Nobody else can report: no token, or one the engine never issued.
    let anonymous = reqwest::Client::new();
    for bearer in [None, Some("not-a-broker-token")] {
        let mut request = anonymous
            .post(format!("{base}/api/agent-auth/conversation"))
            .json(&json!({ "agent": "claude", "id": first }));
        if let Some(bearer) = bearer {
            request = request.bearer_auth(bearer);
        }
        assert_eq!(
            request.send().await.unwrap().status(),
            StatusCode::UNAUTHORIZED
        );
    }
}

#[tokio::test]
async fn a_claude_session_starts_with_its_directory_trusted_and_its_brief_readable() {
    let (tmp, mut cfg, stub) = hibernation_sandbox();
    let claude_home = tmp.path().join("claude-home");
    cfg.agent_onboarding = vogt_engine_server::claude_config::Onboarding {
        enabled: true,
        default_dir: Some(claude_home.clone()),
        settings_file: None,
        opencode_config: None,
    };
    let state_dir = cfg.state_dir.clone();
    let workspace = cfg.workspace_root.clone();
    let (base, _h) = boot_with_config(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let created: Value = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({
            "name": "trusted",
            "prompt": "## Task\n\nDo it.\n",
            "command": [stub.to_string_lossy()],
            "cwd": workspace.to_string_lossy(),
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = created["id"].as_str().unwrap().to_string();
    let printed = live_output_containing(&client, &base, &id, &["helper=["]).await;
    let brief_dir = state_dir.join("agent-task-prompts").join("sessions");
    assert!(
        printed.contains(&format!(
            "arg=[--add-dir={}]\r\narg=[Vogt started",
            brief_dir.display()
        )),
        "the brief's directory is added in the `=` form, right before the prompt: {printed:?}"
    );

    let config: Value =
        serde_json::from_slice(&std::fs::read(claude_home.join(".claude.json")).unwrap()).unwrap();
    let project = &config["projects"][workspace.to_string_lossy().as_ref()];
    assert_eq!(project["hasTrustDialogAccepted"], true, "{config}");
    assert_eq!(
        project["hasClaudeMdExternalIncludesApproved"], true,
        "{config}"
    );

    // A wake resumes; it neither re-sends the brief nor needs its directory.
    client
        .post(format!("{base}/api/sessions/{id}/hibernate"))
        .send()
        .await
        .unwrap();
    client
        .post(format!("{base}/api/sessions/{id}/wake"))
        .send()
        .await
        .unwrap();
    let printed =
        live_output_containing(&client, &base, &id, &["arg=[--resume]", "helper=["]).await;
    let woken = &printed[printed.rfind("arg=[--resume]").unwrap()..];
    assert!(
        !woken.contains("--add-dir") && !woken.contains("Vogt started"),
        "{woken:?}"
    );
}

#[tokio::test]
async fn the_sweep_returns_every_session_with_its_screen_tail_in_one_call() {
    let (tmp, cfg, _stub) = hibernation_sandbox();
    let (base, _h) = boot_with_config(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let quiet = "#!/bin/sh\nsleep 300 &\nprintf 'line one\\n\\nhelper=[%s]\\n> ' \"$!\"\nwait\n";
    let ids = start_stub_agents(
        &client,
        &base,
        &tmp.path().join("agents"),
        &[("awake", quiet), ("asleep", quiet)],
    )
    .await;
    client
        .post(format!("{base}/api/sessions/{}/hibernate", ids[1]))
        .send()
        .await
        .unwrap();
    let ended: Value = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({ "name": "ended", "command": ["/bin/sh", "-c", "true"] }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;

    let rows: Vec<Value> = client
        .get(format!("{base}/api/sessions/sweep?screen_lines=2"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let ids_seen: Vec<&str> = rows
        .iter()
        .map(|r| r["summary"]["id"].as_str().unwrap())
        .collect();
    assert!(ids_seen.contains(&ids[0].as_str()) && ids_seen.contains(&ids[1].as_str()));
    assert!(
        !ids_seen.contains(&ended["id"].as_str().unwrap()),
        "exited is left out"
    );
    for row in &rows {
        let tail: Vec<&str> = row["screen_tail"]
            .as_array()
            .unwrap()
            .iter()
            .map(|l| l.as_str().unwrap())
            .collect();
        assert_eq!(tail.len(), 2, "{row}");
        assert!(
            tail[0].starts_with("helper=["),
            "blank lines skipped: {tail:?}"
        );
    }
    let asleep = rows
        .iter()
        .find(|r| r["summary"]["id"] == ids[1].as_str())
        .unwrap();
    assert_eq!(asleep["summary"]["activity"], "hibernated");
    assert_eq!(
        client
            .get(format!("{base}/api/sessions/sweep?screen_lines=41"))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn a_sessions_process_tree_is_measured_and_rides_on_its_summary() {
    use vogt_engine_server::resources::Sampler;

    let (tmp, cfg, _stub) = hibernation_sandbox();
    let (base, state, _h) = boot_with_state(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    // A tree of three: the stub, a child holding ~32 MiB, and a sleeper.
    let script = "#!/bin/sh\nsleep 300 &\n\
                  python3 -c 'import time; b=bytearray(32<<20); time.sleep(300)' &\n\
                  printf 'helper=[%s]\\n> ' \"$!\"\nwait\n";
    let ids = start_stub_agents(
        &client,
        &base,
        &tmp.path().join("agents"),
        &[("heavy", script)],
    )
    .await;
    let mut events = state.bus.subscribe();
    let mut sampler = Sampler::new(Some(16 << 20));
    // Until the child has allocated its 32 MiB; a loaded runner is slow to
    // start python.
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    loop {
        sampler.run_once(&state).await;
        let session = state.sessions.get(ids[0].parse().unwrap()).unwrap();
        let rss = session.summary().resources.map_or(0, |r| r.rss_bytes);
        if rss >= 32 << 20 || std::time::Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    let listed: Vec<Value> = client
        .get(format!("{base}/api/sessions"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let row = listed.iter().find(|s| s["id"] == ids[0].as_str()).unwrap();
    let resources = &row["resources"];
    assert!(
        resources["rss_bytes"].as_u64().unwrap() >= 32 << 20,
        "{resources}"
    );
    assert!(resources["processes"].as_u64().unwrap() >= 3, "{resources}");
    assert_eq!(resources["over_threshold"], true);
    let event = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let Ok(ServerEvent::SessionResources { samples }) = events.recv().await {
                return samples;
            }
        }
    })
    .await
    .unwrap();
    assert!(event.iter().any(|s| s.id.to_string() == ids[0]));
}

/// WI-920: a client that falls behind the event stream is told so in band,
/// the stream keeps going, and the lag is on record in `/api/status`.
#[tokio::test]
async fn a_lagging_event_stream_is_told_and_keeps_going() {
    let (base, state, _h) = boot_with_state(test_config()).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let mut response = client
        .get(format!("{base}/api/events"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    // Large events, published while nothing reads the response: the socket
    // fills, the stream stops being polled, and its receiver falls more than
    // the bus's capacity behind.
    let padding = "x".repeat(16 * 1024);
    for seq in 0..2000 {
        state.bus.publish(ServerEvent::VogtChanged {
            kind: "test.burst".into(),
            entity_kind: "work_item".into(),
            entity_id: "WI-1".into(),
            seq,
            summary: json!({ "padding": padding }),
        });
    }
    let mut seen = String::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    while !seen.contains("\"type\":\"lagged\"") {
        let chunk = tokio::time::timeout_at(deadline, response.chunk())
            .await
            .expect("the lagged notice never arrived")
            .unwrap()
            .expect("the stream ended");
        // Keep only a tail: the burst is tens of megabytes.
        seen.push_str(&String::from_utf8_lossy(&chunk));
        if seen.len() > 64 * 1024 {
            seen = seen[seen.len() - 64 * 1024..].to_string();
        }
    }
    // The stream is still alive after the lag.
    state.bus.publish(ServerEvent::SessionRenamed {
        id: uuid::Uuid::nil(),
        name: "after the lag".into(),
    });
    let mut after = String::new();
    while !after.contains("after the lag") {
        let chunk = tokio::time::timeout_at(deadline, response.chunk())
            .await
            .expect("nothing arrived after the lag")
            .unwrap()
            .expect("the stream ended");
        after.push_str(&String::from_utf8_lossy(&chunk));
        if after.len() > 64 * 1024 {
            after = after[after.len() - 64 * 1024..].to_string();
        }
    }
    let status: Value = client
        .get(format!("{base}/api/status"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        status["event_lag"]["sse-events"]["events_skipped"]
            .as_u64()
            .unwrap()
            > 0,
        "{status}"
    );
}

/// WI-917: a startup gate shows as `awaiting-approval` with its kind and
/// options, and is answered by choice, not by counting arrow presses.
#[tokio::test]
async fn a_trust_gate_is_reported_with_its_options_and_answered_by_choice() {
    let (tmp, cfg, _stub) = hibernation_sandbox();
    let (base, _h) = boot_with_config(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    // A menu that redraws its highlight on arrows and reports the choice.
    let gate = tmp.path().join("gate.py");
    std::fs::write(
        &gate,
        r#"import sys, termios, tty, os
opts = ["Yes, I trust this folder", "No, exit"]
sel = 0
def draw():
    sys.stdout.write("\x1b[2J\x1b[H")
    sys.stdout.write("Quick safety check: Is this a project you created or one you trust?\r\n\r\n")
    for i, o in enumerate(opts):
        sys.stdout.write(("❯ " if i == sel else "  ") + f"{i+1}. {o}\r\n")
    sys.stdout.flush()
fd = sys.stdin.fileno()
tty.setraw(fd)
draw()
buf = b""
while True:
    buf += os.read(fd, 16)
    while buf:
        if buf.startswith(b"\x1b[B"):
            sel = min(sel + 1, len(opts) - 1); buf = buf[3:]; draw()
        elif buf.startswith(b"\x1b[A"):
            sel = max(sel - 1, 0); buf = buf[3:]; draw()
        elif buf.startswith(b"\r"):
            sys.stdout.write("\x1b[2J\x1b[H" + f"chose {sel+1}\r\n> ")
            sys.stdout.flush()
            os.read(fd, 1)
            sys.exit(0)
        elif buf.startswith(b"\x1b") and len(buf) < 3:
            break
        else:
            buf = buf[1:]
"#,
    )
    .unwrap();
    let created: Value = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({ "name": "gated", "command": ["python3", gate.to_string_lossy()] }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = created["id"].as_str().unwrap().to_string();

    // It reads as awaiting-approval, with the gate's kind and its menu.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    let approval = loop {
        let screen: Value = client
            .get(format!("{base}/api/sessions/{id}/screen"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        if screen["activity"] == "awaiting-approval" {
            break screen["approval"].clone();
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "never awaiting-approval: {screen}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    assert_eq!(approval["kind"], "folder-trust", "{approval}");
    assert_eq!(approval["options"][1]["label"], "No, exit");
    assert_eq!(approval["options"][0]["selected"], true);

    // A stale question is refused and nothing is typed.
    let stale = client
        .post(format!("{base}/api/sessions/{id}/answer"))
        .json(&json!({ "option": 2, "expect_question": "Do you want to proceed?" }))
        .send()
        .await
        .unwrap();
    assert_eq!(stale.status(), StatusCode::CONFLICT);

    // Answer by label: the engine moves down one and presses Enter.
    let answered: Value = client
        .post(format!("{base}/api/sessions/{id}/answer"))
        .json(&json!({ "label": "no, exit" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(answered["chosen"]["number"], 2, "{answered}");
    assert_eq!(answered["kind"], "folder-trust");
    assert_eq!(answered["dismissed"], true, "{answered}");
    live_output_containing(&client, &base, &id, &["chose 2"]).await;

    let none = client
        .post(format!("{base}/api/sessions/{id}/answer"))
        .json(&json!({ "option": 1 }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        none.status(),
        StatusCode::CONFLICT,
        "no dialog left to answer"
    );
}

/// WI-926: a session started with a posture gets it, keeps it across a wake,
/// shows it on its summary, and every Claude launch carries the policy file.
#[tokio::test]
async fn a_permission_posture_reaches_the_agent_and_survives_a_wake() {
    let (tmp, mut cfg, stub) = hibernation_sandbox();
    let policy = tmp.path().join("policy.json");
    std::fs::write(&policy, br#"{"autoMode":{"allow":["$defaults"]}}"#).unwrap();
    cfg.agent_onboarding.settings_file = Some(policy.clone());
    let (base, _h) = boot_with_config(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let created: Value = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({
            "name": "trusted task",
            "command": [stub.to_string_lossy()],
            "permission_mode": "bypass",
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = created["id"].as_str().unwrap().to_string();
    assert_eq!(created["permission_mode"], "bypass");
    let printed = live_output_containing(&client, &base, &id, &["helper=["]).await;
    assert!(
        printed.contains("arg=[--dangerously-skip-permissions]"),
        "{printed:?}"
    );
    assert!(
        printed.contains(&format!("arg=[--settings={}]", policy.display())),
        "{printed:?}"
    );

    client
        .post(format!("{base}/api/sessions/{id}/hibernate"))
        .send()
        .await
        .unwrap();
    client
        .post(format!("{base}/api/sessions/{id}/wake"))
        .send()
        .await
        .unwrap();
    let printed =
        live_output_containing(&client, &base, &id, &["arg=[--resume]", "helper=["]).await;
    let woken = &printed[printed.rfind("arg=[--resume]").unwrap()..];
    assert!(
        woken.contains("--dangerously-skip-permissions"),
        "kept on wake: {woken:?}"
    );

    let refused = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({ "name": "shell", "permission_mode": "bypass" }))
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), StatusCode::BAD_REQUEST);
}

/// WI-913: a session stopped on request reads `stopped` with who and why;
/// one that dies by itself still reads `errored`.
#[tokio::test]
async fn a_requested_stop_is_stopped_and_a_crash_is_still_errored() {
    let (base, _h) = boot().await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let start = |name: &'static str, command: Vec<&'static str>| {
        let client = client.clone();
        let base = base.clone();
        async move {
            let created: Value = client
                .post(format!("{base}/api/sessions"))
                .json(&json!({ "name": name, "command": command }))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            created["id"].as_str().unwrap().to_string()
        }
    };
    let reaped = start("reaped child", vec!["/bin/sh", "-c", "sleep 300"]).await;
    let crashed = start("crashing child", vec!["/bin/sh", "-c", "sleep 0.2; exit 3"]).await;
    tokio::time::sleep(Duration::from_millis(150)).await;
    let killed = client
        .post(format!("{base}/api/sessions/{reaped}/kill"))
        .json(&json!({ "reason": "answer ingested", "by": "agent:session:ses_parent" }))
        .send()
        .await
        .unwrap()
        .status();
    assert_eq!(killed, StatusCode::OK);

    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let listed: Vec<Value> = client
            .get(format!("{base}/api/sessions"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let row = |id: &str| listed.iter().find(|s| s["id"] == id).unwrap().clone();
        let (r, c) = (row(&reaped), row(&crashed));
        if r["alive"] == false && c["alive"] == false {
            assert_eq!(r["activity"], "stopped", "{r}");
            assert_eq!(r["stop"]["reason"], "answer ingested");
            assert_eq!(r["stop"]["by"], "agent:session:ses_parent");
            assert!(r["stop"]["at"].as_str().is_some_and(|a| !a.is_empty()));
            assert_eq!(c["activity"], "errored", "a crash stays a crash: {c}");
            assert!(c.get("stop").is_none());
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "never exited: {r} {c}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// WI-927: a session's launch wrapper reports its stages to the engine, which
/// accepts the report once, from that session's own broker token only, and
/// keeps the timings as metrics — the real `report_launch` from
/// `agent-auth.sh`, not a hand-built request.
#[tokio::test]
async fn a_launch_report_from_the_wrapper_is_accepted_once_and_measured() {
    if std::process::Command::new("jq")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("skipping: the launch report is built with jq");
        return;
    }
    let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../deploy/agent-auth.sh");
    let tmp = tempfile::tempdir().unwrap();
    let cfg = broker_config(&tmp, "LATER proj later ondemand\n");
    let (base, state, _h) = boot_with_state(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let launch = format!(
        "source '{}'; begin_launch shell; launch_stage login 12 ok; \
         PROJECT_MODE[p-1]=bulk; PROJECT_MS[p-1]=40; PROJECT_OUTCOME[p-1]=ok; \
         PROJECT_NAMES[p-1]='GH:GITHUB_PAT:1 X:MISSING_ONE:0'; \
         launch_stage bootstrap 250 ok; report_launch ok; echo reported-$?; sleep 30",
        script.display()
    );
    let id = client
        .post(format!("{base}/api/sessions"))
        // The harness binds an ephemeral port, so the broker's advertised
        // loopback address (from the configured bind) is pointed at it here.
        .json(&json!({
            "name": "launch-report",
            "command": ["/bin/bash", "-c", launch],
            "env": [["VOGT_ENGINE_BROKER_URL", base]],
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
    live_output_containing(&client, &base, &id, &["reported-0"]).await;

    // Accepted: the session's one report is now claimed.
    let session = state.sessions.get(id.parse().unwrap()).unwrap();
    assert!(
        !session.mark_launch_reported(),
        "the wrapper's report should have been accepted"
    );
    let text = vogt_engine_server::metrics::metrics().render();
    for expected in [
        "vogt_session_launch_seconds_count{command=\"shell\",outcome=\"ok\"}",
        "vogt_session_launch_stage_seconds_count{stage=\"login\"}",
        "vogt_session_launch_stage_seconds_count{stage=\"secrets\"}",
        "vogt_session_launch_stage_seconds_count{stage=\"bootstrap\"}",
        "vogt_session_launch_secret_reads_total{mode=\"bulk\",outcome=\"ok\"}",
        "vogt_session_first_output_seconds_count{launcher=\"direct\"}",
        "vogt_session_starts_total{origin=\"api\",outcome=\"ok\"}",
    ] {
        assert!(text.contains(expected), "missing {expected} in\n{text}");
    }

    // Nobody else can report: no token, or a token the engine never issued.
    let anonymous = reqwest::Client::new();
    for bearer in [None, Some("not-a-broker-token")] {
        let mut request = anonymous
            .post(format!("{base}/api/agent-auth/launch-report"))
            .json(&json!({ "outcome": "ok", "total_ms": 1 }));
        if let Some(bearer) = bearer {
            request = request.bearer_auth(bearer);
        }
        assert_eq!(
            request.send().await.unwrap().status(),
            StatusCode::UNAUTHORIZED
        );
    }
}

/// WI-927: `/metrics` is served on its own listener, never on the API port.
#[tokio::test]
async fn metrics_are_served_on_their_own_listener_only() {
    let (base, _h) = boot().await;
    // The API port answers /metrics with the GUI shell, never the metrics.
    let on_api = reqwest::Client::new()
        .get(format!("{base}/metrics"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(
        !on_api.contains("vogt_session_"),
        "the API port must not serve /metrics"
    );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, vogt_engine_server::metrics::router())
            .await
            .unwrap()
    });
    let response = reqwest::get(format!("http://{addr}/metrics"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(response
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap()
        .starts_with("text/plain; version=0.0.4"));
    let body = response.text().await.unwrap();
    assert!(
        body.contains("# TYPE vogt_session_first_output_seconds histogram"),
        "{body}"
    );
}

/// WI-934: `stopped` is keyed on the stop request, not on the CLI, so a
/// stopped opencode session reads `stopped` too, and one that dies on its
/// own still reads `errored`.
#[tokio::test]
async fn a_stopped_opencode_session_is_stopped_not_errored() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    // A stand-in named `opencode`, so the engine treats it as that CLI.
    let opencode = tmp.path().join("opencode");
    std::fs::write(
        &opencode,
        "#!/bin/sh\n[ \"$1\" = crash ] && { sleep 0.2; exit 7; }\nsleep 300\n",
    )
    .unwrap();
    std::fs::set_permissions(&opencode, std::fs::Permissions::from_mode(0o755)).unwrap();
    let (base, _h) = boot().await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let mut ids = Vec::new();
    for args in [vec![], vec!["crash"]] {
        let mut command = vec![opencode.to_string_lossy().to_string()];
        command.extend(args.iter().map(|a| a.to_string()));
        let created: Value = client
            .post(format!("{base}/api/sessions"))
            .json(&json!({ "name": "opencode", "command": command }))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        ids.push(created["id"].as_str().unwrap().to_string());
    }
    let (stopped, crashed) = (&ids[0], &ids[1]);
    tokio::time::sleep(Duration::from_millis(150)).await;
    let status = client
        .post(format!("{base}/api/sessions/{stopped}/kill"))
        .json(&json!({ "reason": "packet done", "by": "agent:session:ses_driver" }))
        .send()
        .await
        .unwrap()
        .status();
    assert_eq!(status, StatusCode::OK);
    let row = wait_for_session_row(&client, &base, stopped, |s| !s["exit_code"].is_null()).await;
    assert_eq!(row["activity"], "stopped", "{row:?}");
    assert_eq!(row["stop"]["reason"], "packet done", "{row:?}");
    let row = wait_for_session_row(&client, &base, crashed, |s| s["exit_code"] == json!(7)).await;
    assert_eq!(row["activity"], "errored", "{row:?}");
}

/// A session started with no `cwd` runs in the engine's default directory,
/// which on the pods is `~`, outside the workspace root. Hibernating it
/// recorded that directory and waking it replayed it as a requested `cwd`,
/// which was refused ("path escapes workspace_root"): every GUI or template
/// session hibernated for good. Found validating WI-912 on vogt-dev.
#[tokio::test]
async fn a_session_in_the_default_directory_outside_the_workspace_wakes() {
    let (tmp, mut cfg, stub) = hibernation_sandbox();
    let home = tmp.path().join("home");
    let workspace = tmp.path().join("ws");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    cfg.default_cwd = home.clone();
    cfg.workspace_root = workspace.canonicalize().unwrap();
    let (base, _state, _h) = boot_with_state(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let created: Value = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({ "name": "home", "command": [stub.to_string_lossy()] }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = created["id"].as_str().unwrap().to_string();
    assert_eq!(created["cwd"], home.to_string_lossy().as_ref());
    live_output_containing(&client, &base, &id, &["helper=["]).await;
    let hibernated = client
        .post(format!("{base}/api/sessions/{id}/hibernate"))
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        hibernated.status(),
        StatusCode::OK,
        "{:?}",
        hibernated.text().await
    );
    let woken = client
        .post(format!("{base}/api/sessions/{id}/wake"))
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    let status = woken.status();
    let body: Value = woken.json().await.unwrap();
    assert_eq!(status, StatusCode::OK, "{body}");
    let row = wait_for_session_row(&client, &base, &id, |s| s["alive"] == json!(true)).await;
    assert_eq!(row["cwd"], home.to_string_lossy().as_ref(), "{row:?}");
}

/// WI-926: an agent session the engine starts itself is given its own agent
/// credential, minted by vogt-core (`POST /api/sessions/token`), instead of
/// running with the pod's token, which is bound to a person. A plain shell
/// keeps the pod's; a session that arrives with a credential of its own (one
/// vogt-core started) is left alone; the credential is revoked when the
/// session ends.
#[tokio::test]
async fn an_engine_started_agent_session_gets_its_own_credential() {
    use axum::{extract::State, routing::post, Router};
    use std::os::unix::fs::PermissionsExt;
    type Calls = Arc<std::sync::Mutex<Vec<Value>>>;
    let calls: Calls = Arc::default();
    async fn token(
        State(calls): State<Calls>,
        axum::Json(body): axum::Json<Value>,
    ) -> axum::Json<Value> {
        calls.lock().unwrap().push(body.clone());
        let id = body["engine_session_id"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        axum::Json(json!({
            "engine_session_id": id,
            "actor": format!("agent:engine:{id}"),
            "token": if body["revoke"] == json!(true) { Value::Null } else { json!(format!("minted-for-{id}")) },
            "revoked": 0,
        }))
    }
    let app = Router::new()
        .route("/api/sessions/token", post(token))
        .with_state(Arc::clone(&calls));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let core_addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    let tmp = tempfile::tempdir().unwrap();
    // A stand-in named `claude`, so the engine treats it as that agent CLI.
    let claude = tmp.path().join("claude");
    std::fs::write(
        &claude,
        "#!/bin/sh\nprintf 'tok=[%s] sid=[%s]\\n' \"$VOGT_HTTP_TOKEN\" \"$VOGT_SESSION_ID\"\nsleep 300\n",
    )
    .unwrap();
    std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut cfg = test_config();
    cfg.vogt_core_url = Some(format!("http://{core_addr}"));
    cfg.vogt_core_token = Some("stack-secret".into());
    let (base, _state, _h) = boot_with_state(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let start = |body: Value| {
        let client = client.clone();
        let base = base.clone();
        async move {
            let created: Value = client
                .post(format!("{base}/api/sessions"))
                .json(&body)
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            created["id"].as_str().unwrap().to_string()
        }
    };

    let agent = start(json!({ "name": "agent", "command": [claude.to_string_lossy()] })).await;
    let printed = live_output_containing(&client, &base, &agent, &["tok=["]).await;
    assert!(
        printed.contains(&format!("tok=[minted-for-{agent}] sid=[{agent}]")),
        "{printed}"
    );

    let shell = start(json!({ "name": "shell", "command": ["/bin/sh", "-c", "printf 'tok=[%s]\\n' \"$VOGT_HTTP_TOKEN\"; sleep 300"] })).await;
    let printed = live_output_containing(&client, &base, &shell, &["tok=["]).await;
    assert!(
        printed.contains("tok=[]"),
        "a shell is not minted for: {printed}"
    );

    let core_started = start(json!({
        "name": "core-started",
        "command": [claude.to_string_lossy()],
        "env": [["VOGT_HTTP_TOKEN", "the-cores-own"], ["VOGT_SESSION_ID", "ses_1"]],
    }))
    .await;
    let printed = live_output_containing(&client, &base, &core_started, &["tok=["]).await;
    assert!(
        printed.contains("tok=[the-cores-own] sid=[ses_1]"),
        "{printed}"
    );

    let minted: Vec<Value> = calls.lock().unwrap().clone();
    assert_eq!(minted.len(), 1, "only the engine-started agent: {minted:?}");
    assert_eq!(minted[0]["engine_session_id"], agent.as_str());

    // The credential ends with the session.
    client
        .post(format!("{base}/api/sessions/{agent}/kill"))
        .send()
        .await
        .unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        if calls
            .lock()
            .unwrap()
            .iter()
            .any(|c| c["revoke"] == json!(true) && c["engine_session_id"] == agent.as_str())
        {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "no revoke: {:?}",
            calls.lock().unwrap()
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// WI-949: a driven opencode session on autopilot works through a backlog
/// unattended. The stand-in draws opencode's own idle composer (the `┃` bar
/// and `╹▀` footer, no prompt glyph), takes one backlog item per line of
/// input, and prints `AUTOPILOT: DONE` after the last. Nothing in this test
/// types into it: every item after the start is the engine's nudge.
#[tokio::test]
async fn an_autopilot_opencode_session_advances_through_its_backlog_unattended() {
    use std::os::unix::fs::PermissionsExt;
    const ITEMS: usize = 3;

    let tmp = tempfile::tempdir().unwrap();
    let log = tmp.path().join("done.log");
    let bin = tmp.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let opencode = bin.join("opencode");
    std::fs::write(
        &opencode,
        format!(
            r#"#!/usr/bin/env bash
box() {{ printf '  ┃\r\n  ┃  Ask anything…\r\n  ┃\r\n  ╹▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀\r\n   tab agents  ctrl+p commands\r\n'; }}
n=0
box
while IFS= read -r _; do
  n=$((n+1))
  echo "item $n" >> '{log}'
  printf '     ▣  Build · stand-in · item %s done\r\n' "$n"
  if [ "$n" -ge {ITEMS} ]; then printf '     AUTOPILOT: DONE\r\n'; fi
  box
done
"#,
            log = log.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&opencode, std::fs::Permissions::from_mode(0o755)).unwrap();

    let mut cfg = test_config();
    cfg.autopilot = vogt_engine_server::autopilot::Policy {
        nudge_after: Duration::from_millis(600),
        max_nudges: 10,
    };
    let (base, _h) = boot_with_config(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let created: Value = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({
            "name": "backlog",
            "command": [opencode.display().to_string()],
            "autopilot": true,
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = created["id"].as_str().unwrap().to_string();
    assert_eq!(created["autopilot"], true);

    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    let summary = loop {
        let detail: Value = client
            .get(format!("{base}/api/sessions/{id}"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let summary = detail["summary"].clone();
        if summary.get("autopilot").is_none() {
            break summary;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "autopilot never finished: {summary}"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    let done = std::fs::read_to_string(&log).unwrap_or_default();
    assert_eq!(
        done.lines().count(),
        ITEMS,
        "one item per nudge, and none after DONE: {done:?}"
    );
    assert_eq!(summary["autopilot_nudges"], ITEMS as u64);

    // Done is final: no further nudge however long it sits there.
    tokio::time::sleep(Duration::from_millis(1_500)).await;
    let done = std::fs::read_to_string(&log).unwrap_or_default();
    assert_eq!(done.lines().count(), ITEMS);
}

/// A session blocked on a person is never nudged, autopilot or not.
#[tokio::test]
async fn an_autopilot_session_blocked_on_a_person_is_left_alone() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let log = tmp.path().join("input.log");
    let bin = tmp.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let opencode = bin.join("opencode");
    std::fs::write(
        &opencode,
        format!(
            "#!/usr/bin/env bash\nprintf '  ┃\\r\\n  ╹▀▀▀▀▀▀▀▀\\r\\n'\nwhile IFS= read -r line; do echo x >> '{}'; done\n",
            log.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&opencode, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut cfg = test_config();
    cfg.autopilot = vogt_engine_server::autopilot::Policy {
        nudge_after: Duration::from_millis(400),
        max_nudges: 10,
    };
    let (base, _h) = boot_with_config(cfg).await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let created: Value = client
        .post(format!("{base}/api/sessions"))
        .json(&json!({
            "name": "blocked",
            "command": [opencode.display().to_string()],
            "autopilot": true,
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = created["id"].as_str().unwrap().to_string();
    let status = client
        .post(format!("{base}/api/sessions/{id}/blocked"))
        .json(&json!({"blocked": true, "reason": "needs a person"}))
        .send()
        .await
        .unwrap()
        .status();
    assert!(status.is_success());
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert!(
        std::fs::read_to_string(&log).unwrap_or_default().is_empty(),
        "a blocked session was nudged"
    );
}

/// WI-1005: every route that returns file content holds to the one policy
/// in `workspace_path::may_show` — the viewer, the download, the ripgrep
/// search and the git diff — judged on the resolved path, so neither a
/// symlink nor a rename walks a credential past it.
#[tokio::test]
async fn file_content_routes_refuse_hidden_and_secret_files() {
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
    std::fs::create_dir(repo.join(".github")).unwrap();
    std::fs::write(repo.join(".github/ci.yml"), "on: push\n").unwrap();
    std::fs::write(repo.join("server.key"), "needle-key\n").unwrap();
    std::fs::write(repo.join("ok.txt"), "needle-ok\n").unwrap();
    sh("git add .github/ci.yml server.key ok.txt && git commit -q -m init");
    std::fs::write(repo.join(".github/ci.yml"), "on: [push]\n").unwrap();
    std::fs::write(repo.join("prod.env"), "needle-env\n").unwrap();
    std::fs::write(repo.join(".envrc"), "needle-envrc\n").unwrap();
    std::os::unix::fs::symlink("prod.env", repo.join("link.txt")).unwrap();

    let (base, _h) = boot_with_config(Config {
        default_cwd: repo.to_path_buf(),
        workspace_root: repo.canonicalize().unwrap(),
        ..test_config()
    })
    .await;
    let client = reqwest::Client::builder()
        .default_headers(auth())
        .build()
        .unwrap();
    let status = |path: String| {
        let client = client.clone();
        async move { client.get(path).send().await.unwrap().status() }
    };

    // Viewer and download: secret names, hidden components, and a link that
    // resolves to a secret are all refused; an ordinary file still reads.
    for path in [
        "prod.env",
        "server.key",
        ".envrc",
        ".git/config",
        ".github/ci.yml",
        "link.txt",
    ] {
        for route in ["files", "files/download"] {
            assert_eq!(
                status(format!("{base}/api/{route}?path={path}")).await,
                StatusCode::BAD_REQUEST,
                "/api/{route} must refuse {path}"
            );
        }
    }
    assert_eq!(
        status(format!("{base}/api/files?path=ok.txt")).await,
        StatusCode::OK
    );

    // Search: hits in secret files are dropped; a hidden search root is refused.
    let hits: Vec<Value> = client
        .get(format!("{base}/api/search?q=needle"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let paths: Vec<&str> = hits.iter().filter_map(|h| h["path"].as_str()).collect();
    assert_eq!(
        paths,
        vec!["ok.txt"],
        "search leaked a secret file: {hits:?}"
    );
    assert_eq!(
        status(format!("{base}/api/search?q=.&path=.git")).await,
        StatusCode::BAD_REQUEST
    );

    // Git diff: credential names refused on both sides (server.key is
    // committed), untracked dotfiles and links to secrets refused, a tracked
    // dotfile still diffs.
    for path in [
        "prod.env",
        "server.key",
        ".envrc",
        ".git/config",
        "link.txt",
    ] {
        assert_eq!(
            status(format!("{base}/api/git/diff?path={path}")).await,
            StatusCode::BAD_REQUEST,
            "/api/git/diff must refuse {path}"
        );
    }
    let diff: Value = client
        .get(format!("{base}/api/git/diff?path=.github/ci.yml"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(diff["head"], "on: push\n");
    assert_eq!(diff["current"], "on: [push]\n");

    // Rename-then-read: a secret cannot be moved or copied to an ordinary name.
    for op in ["move", "duplicate"] {
        let res = client
            .post(format!("{base}/api/files/op"))
            .json(&json!({ "op": op, "from": "prod.env", "to": "notes.txt" }))
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST, "{op} of a secret");
    }
    assert!(!repo.join("notes.txt").exists());
}

/// WI-1020: reading the workspace — the viewer, the download, a listing, the
/// tree, both searches and every git read — needs the `sessions` capability.
/// A `read`-only device token and a zero-scope credential are refused before
/// the handler runs; a writer and the stack secret still read.
#[tokio::test]
async fn workspace_reads_need_the_sessions_capability() {
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
    std::fs::write(repo.join("ok.txt"), "needle-ok\n").unwrap();
    sh("git add ok.txt && git commit -q -m init");
    std::fs::write(repo.join("ok.txt"), "needle-ok\nmore\n").unwrap();

    let core = stand_in_core_knowing(vec![
        (
            "reader-token-1234567890abcdef",
            "human:reader",
            vec!["read"],
        ),
        ("noscope-token-1234567890abcdef", "human:nobody", vec![]),
        (
            "writer-token-1234567890abcdef",
            "human:writer",
            vec!["read", "work.write"],
        ),
    ])
    .await;
    let mut cfg = test_config();
    cfg.default_cwd = repo.to_path_buf();
    cfg.workspace_root = repo.canonicalize().unwrap();
    cfg.vogt_core_url = Some(core);
    let (base, _h) = boot_with_config(cfg).await;

    let routes = [
        "files?path=ok.txt",
        "files/download?path=ok.txt",
        "dir",
        "tree?depth=1",
        "search?q=needle",
        "search/files?q=ok",
        "git/status",
        "git/diff?path=ok.txt",
        "git/log?n=5",
        "git/branch",
    ];
    let status = |token: &'static str, route: &'static str| {
        let url = format!("{base}/api/{route}");
        async move {
            reqwest::Client::new()
                .get(url)
                .headers(auth_for(token))
                .send()
                .await
                .unwrap()
                .status()
        }
    };
    for route in routes {
        for token in [
            "reader-token-1234567890abcdef",
            "noscope-token-1234567890abcdef",
        ] {
            assert_eq!(
                status(token, route).await,
                StatusCode::FORBIDDEN,
                "{token} must not read /api/{route}"
            );
        }
        for token in ["writer-token-1234567890abcdef", TEST_TOKEN] {
            assert_eq!(
                status(token, route).await,
                StatusCode::OK,
                "{token} must still read /api/{route}"
            );
        }
    }
}

/// The streaming upload needs `filesystem-write`, like every other write of
/// the tree: a read-only token is refused and nothing lands on disk, while a
/// writer's upload succeeds.
#[tokio::test]
async fn streaming_upload_needs_filesystem_write() {
    let tmp = tempfile::tempdir().unwrap();
    let core = stand_in_core_knowing(vec![
        (
            "reader-token-1234567890abcdef",
            "human:reader",
            vec!["read"],
        ),
        (
            "writer-token-1234567890abcdef",
            "human:writer",
            vec!["read", "work.write"],
        ),
    ])
    .await;
    let mut cfg = test_config();
    cfg.default_cwd = tmp.path().to_path_buf();
    cfg.workspace_root = tmp.path().canonicalize().unwrap();
    cfg.vogt_core_url = Some(core);
    let (base, _h) = boot_with_config(cfg).await;

    let upload = |token: &'static str, name: &'static str| {
        let url = format!("{base}/api/files/upload?path={name}");
        async move {
            reqwest::Client::new()
                .put(url)
                .headers(auth_for(token))
                .body("uploaded\n")
                .send()
                .await
                .unwrap()
                .status()
        }
    };
    assert_eq!(
        upload("reader-token-1234567890abcdef", "reader.txt").await,
        StatusCode::FORBIDDEN
    );
    assert!(!tmp.path().join("reader.txt").exists());
    assert_eq!(
        upload("writer-token-1234567890abcdef", "writer.txt").await,
        StatusCode::OK
    );
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("writer.txt")).unwrap(),
        "uploaded\n"
    );
}
