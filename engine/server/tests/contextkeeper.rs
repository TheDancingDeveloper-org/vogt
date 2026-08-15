//! Continuity integration tests against a fake ContextKeeper.
//!
//! The contract worth pinning is not "the proxy forwards JSON" — it is that a
//! sidecar which is slow, dead, or angry never degrades a terminal, and that
//! its control token never leaves the server.

use std::{
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

use axum::{
    extract::State,
    routing::{get, post},
    Json, Router,
};
use mydevenv2_server::{app::router, Config};
use serde_json::{json, Value};

const TEST_TOKEN: &str = "test-token-1234567890abcdef";
const CONTEXTKEEPER_TOKEN: &str = "contextkeeper-secret-token";

#[derive(Default)]
struct FakeState {
    /// Every Authorization header the fake saw, so a test can prove the token
    /// travelled server-side and only server-side.
    authorizations: Mutex<Vec<String>>,
    pty_id: Mutex<String>,
    circuit_open: AtomicBool,
    launches: AtomicUsize,
}

async fn fake_health() -> Json<Value> {
    Json(json!({
        "status": "ok",
        "capture": {
            "watcher_running": true,
            "first_scan_complete": true,
            "scan_lag_seconds": 1.25,
        }
    }))
}

async fn fake_sessions(State(state): State<Arc<FakeState>>) -> Json<Value> {
    Json(json!([{
        "id": "registry-1",
        "provider": "claude",
        "native_session_id": "native-1",
        "mydevenv2_session_id": state.pty_id.lock().unwrap().clone(),
        "work_id": "work-1",
        "lifecycle": "recovery_pending",
        "event_count": 42,
        "failure_count": 1,
    }]))
}

async fn fake_continuation() -> Json<Value> {
    Json(json!({
        "session_id": "registry-1",
        "work_id": "work-1",
        "primary": {"kind": "resume", "copyable_command": "claude --resume native-1"},
        "alternatives": [{"kind": "bundle", "requires_approval": true}],
    }))
}

async fn fake_preview() -> Json<Value> {
    Json(json!({"bundle_id": "ckb-1", "checksum": "abc", "bundle": "# Recovery"}))
}

async fn fake_approve(Json(body): Json<Value>) -> Json<Value> {
    Json(json!({"bundle_id": body["bundle_id"], "approved_at": "2026-08-11T00:00:00Z"}))
}

async fn fake_launch(
    State(state): State<Arc<FakeState>>,
    Json(_body): Json<Value>,
) -> (axum::http::StatusCode, Json<Value>) {
    state.launches.fetch_add(1, Ordering::SeqCst);
    if state.circuit_open.load(Ordering::SeqCst) {
        return (
            axum::http::StatusCode::TOO_MANY_REQUESTS,
            Json(json!({"detail": {
                "detail": "launch circuit is temporarily open",
                "retry_after": "2026-08-11T00:05:00Z"
            }})),
        );
    }
    (
        axum::http::StatusCode::OK,
        Json(json!({"status": "launched", "child_session_id": "registry-2"})),
    )
}

async fn record_auth(
    State(state): State<Arc<FakeState>>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    if let Some(value) = request.headers().get(axum::http::header::AUTHORIZATION) {
        state
            .authorizations
            .lock()
            .unwrap()
            .push(value.to_str().unwrap_or_default().to_string());
    }
    next.run(request).await
}

async fn boot_fake_contextkeeper() -> (String, Arc<FakeState>) {
    let state = Arc::new(FakeState::default());
    let app = Router::new()
        .route("/healthz", get(fake_health))
        .route("/api/sessions", get(fake_sessions))
        .route("/api/sessions/{id}/continuation", get(fake_continuation))
        .route("/api/sessions/{id}/recovery", get(fake_preview))
        .route("/api/sessions/{id}/recovery/approve", post(fake_approve))
        .route("/api/sessions/{id}/recovery/launch", post(fake_launch))
        .layer(axum::middleware::from_fn_with_state(
            Arc::clone(&state),
            record_auth,
        ))
        .with_state(Arc::clone(&state));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}"), state)
}

fn test_config(contextkeeper_url: Option<String>) -> Config {
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
        state_dir: tempfile::tempdir().unwrap().keep(),
        fcm_service_account_json: None,
        vapid_subject: "mailto:test@example.invalid".to_string(),
        allowed_origins: vec![],
        auto_agent_auth: false,
        agent_auth_helper: "/usr/local/bin/mydevenv2-agent-auth".into(),
        session_templates: vec![],
        assistant_api_key: None,
        assistant_base_url: "https://api.theclawbay.com/v1".into(),
        assistant_model: "gpt-5.4-mini".into(),
        assistant_max_tool_calls: 8,
        assistant_allow_claude_proxy: false,
        assistant_reasoning_effort: None,
        contextkeeper_url,
        contextkeeper_token: contextkeeper_url_token(),
        public_url: None,
        vogt_core_url: None,
        vogt_import_root: None,
        vogt_engine_state_dir: None,
        vogt_core_token: None,
    }
}

fn contextkeeper_url_token() -> Option<String> {
    Some(CONTEXTKEEPER_TOKEN.to_string())
}

async fn boot(cfg: Config) -> String {
    let (router, _state) = router(cfg).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    format!("http://{addr}")
}

fn client() -> reqwest::Client {
    reqwest::Client::new()
}

async fn create_terminal(base: &str) -> String {
    let created: Value = client()
        .post(format!("{base}/api/sessions"))
        .bearer_auth(TEST_TOKEN)
        // `/bin/true` rather than the default interactive shell: this suite is
        // about continuity, and a long-lived bash in a test PTY is a hang
        // waiting to happen (the repo's other integration tests do the same).
        .json(&json!({"name": "term", "command": ["/bin/true"]}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    created["id"].as_str().unwrap().to_string()
}

/// Wait for the background refresher to publish a snapshot, rather than
/// sleeping for a fixed interval and hoping.
///
/// The window has to exceed one full refresh interval: the terminal is created
/// after the server booted, so the first poll ran before there was anything to
/// find and the binding only appears on the next one.
async fn wait_for_continuity(base: &str) -> Value {
    for _ in 0..300 {
        let rows: Value = client()
            .get(format!("{base}/api/sessions"))
            .bearer_auth(TEST_TOKEN)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        if rows[0].get("continuity").is_some() {
            return rows[0].clone();
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("continuity never appeared on the roster");
}

#[tokio::test]
async fn the_roster_reports_protection_state_and_capture_freshness() {
    let (fake_url, fake) = boot_fake_contextkeeper().await;
    let base = boot(test_config(Some(fake_url))).await;
    let terminal = create_terminal(&base).await;
    *fake.pty_id.lock().unwrap() = terminal.clone();

    let row = wait_for_continuity(&base).await;
    let continuity = &row["continuity"];
    assert_eq!(continuity["state"], "recovering", "{continuity}");
    assert_eq!(continuity["provider"], "claude");
    assert_eq!(continuity["session_id"], "registry-1");
    assert_eq!(continuity["work_id"], "work-1");
    assert_eq!(continuity["capture_lag_seconds"], 1.25);
    assert_eq!(continuity["capture_status"], "running");
}

#[tokio::test]
async fn the_browser_never_receives_the_contextkeeper_token() {
    let (fake_url, fake) = boot_fake_contextkeeper().await;
    let base = boot(test_config(Some(fake_url))).await;
    let terminal = create_terminal(&base).await;
    *fake.pty_id.lock().unwrap() = terminal;
    wait_for_continuity(&base).await;

    let body = client()
        .get(format!(
            "{base}/api/contextkeeper/sessions/registry-1/continuation"
        ))
        .bearer_auth(TEST_TOKEN)
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(body.contains("resume"), "{body}");
    assert!(
        !body.contains(CONTEXTKEEPER_TOKEN),
        "token leaked to the client"
    );

    // ...and it did travel server-side, so this is not passing by accident.
    let seen = fake.authorizations.lock().unwrap().clone();
    assert!(
        seen.iter()
            .all(|header| header == &format!("Bearer {CONTEXTKEEPER_TOKEN}")),
        "{seen:?}"
    );
}

#[tokio::test]
async fn continuity_routes_are_absent_when_contextkeeper_is_not_configured() {
    let mut cfg = test_config(None);
    cfg.contextkeeper_token = None;
    let base = boot(cfg).await;
    create_terminal(&base).await;

    let rows: Value = client()
        .get(format!("{base}/api/sessions"))
        .bearer_auth(TEST_TOKEN)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(rows[0].get("continuity").is_none(), "{rows}");

    let health: Value = client()
        .get(format!("{base}/api/contextkeeper/health"))
        .bearer_auth(TEST_TOKEN)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(health["configured"], false);

    let proxied = client()
        .get(format!("{base}/api/contextkeeper/sessions/x/continuation"))
        .bearer_auth(TEST_TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(proxied.status(), reqwest::StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_contextkeeper_outage_leaves_terminals_working_and_unprotected() {
    // A port nothing is listening on: every call fails at connect.
    let dead = {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        format!("http://{addr}")
    };
    let base = boot(test_config(Some(dead))).await;

    // Creating and listing terminals is unaffected.
    let terminal = create_terminal(&base).await;
    let rows: Value = client()
        .get(format!("{base}/api/sessions"))
        .bearer_auth(TEST_TOKEN)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(rows[0]["id"], terminal);
    assert!(
        rows[0].get("continuity").is_none(),
        "unprotected, not broken"
    );

    let health: Value = client()
        .get(format!("{base}/api/contextkeeper/health"))
        .bearer_auth(TEST_TOKEN)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(health["configured"], true);
    assert_eq!(health["reachable"], false);

    // A proxied call reports a gateway failure rather than a server fault.
    let proxied = client()
        .get(format!("{base}/api/contextkeeper/sessions/x/continuation"))
        .bearer_auth(TEST_TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(proxied.status(), reqwest::StatusCode::BAD_GATEWAY);
}

#[tokio::test]
async fn preview_then_approve_then_launch_is_proxied_end_to_end() {
    let (fake_url, fake) = boot_fake_contextkeeper().await;
    let base = boot(test_config(Some(fake_url))).await;

    let preview: Value = client()
        .get(format!(
            "{base}/api/contextkeeper/sessions/registry-1/preview"
        ))
        .bearer_auth(TEST_TOKEN)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(preview["bundle_id"], "ckb-1");

    let approved: Value = client()
        .post(format!(
            "{base}/api/contextkeeper/sessions/registry-1/approve"
        ))
        .bearer_auth(TEST_TOKEN)
        .json(&json!({"bundle_id": "ckb-1"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(approved["bundle_id"], "ckb-1");

    let launched: Value = client()
        .post(format!(
            "{base}/api/contextkeeper/sessions/registry-1/launch"
        ))
        .bearer_auth(TEST_TOKEN)
        .json(&json!({"bundle_id": "ckb-1"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(launched["status"], "launched");
    assert_eq!(launched["child_session_id"], "registry-2");
    assert_eq!(fake.launches.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn an_open_launch_circuit_keeps_its_status_and_retry_time() {
    let (fake_url, fake) = boot_fake_contextkeeper().await;
    fake.circuit_open.store(true, Ordering::SeqCst);
    let base = boot(test_config(Some(fake_url))).await;

    let response = client()
        .post(format!(
            "{base}/api/contextkeeper/sessions/registry-1/launch"
        ))
        .bearer_auth(TEST_TOKEN)
        .json(&json!({"bundle_id": "ckb-1"}))
        .send()
        .await
        .unwrap();
    // The status survives the proxy, so the UI can tell "refused for now" from
    // "broken", and the retry time is there to show.
    assert_eq!(response.status(), reqwest::StatusCode::TOO_MANY_REQUESTS);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["detail"]["retry_after"], "2026-08-11T00:05:00Z");
}

#[tokio::test]
async fn continuity_for_an_unknown_terminal_reads_as_unprotected() {
    let (fake_url, _fake) = boot_fake_contextkeeper().await;
    let base = boot(test_config(Some(fake_url))).await;
    let unknown = uuid::Uuid::new_v4();
    // The fake's roster maps a different (empty) PTY id, so this terminal has
    // no ContextKeeper session bound to it.
    let body: Value = client()
        .get(format!("{base}/api/contextkeeper/terminals/{unknown}"))
        .bearer_auth(TEST_TOKEN)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["state"], "unprotected");
}
