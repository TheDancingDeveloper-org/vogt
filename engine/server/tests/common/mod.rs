//! Shared helpers for the integration tests.

pub use base64::Engine as _;
pub use flate2::read::GzDecoder;
pub use futures_util::{SinkExt, StreamExt};
pub use reqwest::StatusCode;
pub use serde_json::{json, Value};
pub use std::{
    os::unix::fs::PermissionsExt,
    sync::{Arc, Mutex},
    time::Duration,
};
pub use time::OffsetDateTime;
pub use tokio_tungstenite::tungstenite::Message;
pub use uuid::Uuid;
pub use vogt_engine_contract::{ServerEvent, SessionDetail};
pub use vogt_engine_server::{app::router, app::AppState, config::SessionTemplate, Config};

mod more;
pub use more::*;

pub const TEST_TOKEN: &str = "test-token-1234567890abcdef";

pub fn test_config() -> Config {
    Config {
        pod_backends_enabled: false,
        host_placement: Default::default(),
        session_default_backend: "local".into(),
        bind: "127.0.0.1:0".parse().unwrap(),
        token: Some(TEST_TOKEN.into()),
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
        ws_ping_interval: std::time::Duration::from_secs(20),
        // The push tests subscribe a loopback http stand-in service.
        push_allow_insecure_endpoints: true,
        state_dir: tempfile::tempdir().unwrap().keep(),
        fcm_service_account_json: None,
        vapid_subject: "mailto:test@example.invalid".to_string(),
        allowed_origins: vec![],
        auto_agent_auth: false,
        agent_auth_helper: "/usr/local/bin/vogt-agent-auth".into(),
        codex_session_id: false,
        codex_approval_policy: "never".to_string(),
        codex_transcript_root: None,
        agent_auth_secrets: vec![],
        agent_auth_secrets_file: None,
        llm_providers_file: None,
        llm_model_allowlist: vec![],
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
        assistant_starts_per_hour: 3,
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
        agent_auth_public_url: None,
        pod: Default::default(),
        pod_salvage: Default::default(),
        graft: vogt_engine_server::graft_provider::GraftSettings::from_lookup(|_| None),
        workspace_size_ttl: std::time::Duration::from_secs(60),
        escalation_ladder: Default::default(),
        cadastre_mcp_url: None,
        vogt_prod_mcp_url: None,
        lane: None,
        crw_mcp_url: None,
        vogt_core_url: None,
        vogt_import_root: None,
        vogt_engine_state_dir: None,
        vogt_core_token: None,
        agent_clis: vogt_engine_server::agent_clis::AgentCliPaths::default(),
        hibernation: vogt_engine_server::hibernate_policy::Policy::default(),
        session_list_retain_after: None,
        stopped_sessions: Default::default(),
        assistant_call: vogt_engine_server::call::CallPolicy::default(),
        autopilot: vogt_engine_server::autopilot::Policy::default(),
        first_turn: vogt_engine_server::first_turn::Policy::default(),
        budget: vogt_engine_server::budget::Policy::default(),
        agent_onboarding: vogt_engine_server::claude_config::Onboarding::default(),
        session_rss_warn_bytes: None,
        worktree_clean_on_hibernate: false,
        worktree_clean_on_stop: false,
        worktree_remove_on_stop: false,
        disk_warn_percent: 85,
        scratch_prune_bytes: 2 << 30,
        min_free_percent: 0,
        shared_cargo_target_dir: None,
        shared_cargo_target_max_bytes: 40 << 30,
        metrics_bind: None,
        chat: Default::default(),
        economics_tick: std::time::Duration::ZERO,
        compaction_loop_threshold: 0,
        economics_home: None,
    }
}

pub async fn boot() -> (String, ServerGuard) {
    boot_with_config(test_config()).await
}

/// Boot with a chosen scrollback ring size, leaving every other test-config
/// default in place. The suite default stays 64 KiB (`test_config`); the
/// large-session resume tests use this to size the ring around the flood they
/// are exercising (a tiny ring an early cursor ages out of, or the 4 MiB ring
/// prod runs). A per-session `scrollback_bytes` override on session-create also
/// works, but sizing the whole engine keeps each test's intent in one place.
#[allow(dead_code)] // used by the large-session resume tests
pub async fn boot_with(scrollback_bytes: usize) -> (String, ServerGuard) {
    boot_with_config(Config {
        scrollback_bytes,
        ..test_config()
    })
    .await
}

pub async fn boot_with_config(cfg: Config) -> (String, ServerGuard) {
    let (base, _state, handle) = boot_with_state(cfg).await;
    (base, handle)
}

/// Boot and keep the `AppState`, so a test can publish onto the engine's own
/// event bus — the same bus `vogt_core::spawn_event_follower` republishes
/// vogt-core's events onto — and drive the agent-task trigger watcher
/// with synthetic core events, without needing a real vogt-core beside it.
pub async fn boot_with_state(cfg: Config) -> (String, Arc<AppState>, ServerGuard) {
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
pub struct ServerGuard {
    pub server: tokio::task::JoinHandle<()>,
    pub state: Arc<AppState>,
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
pub fn publish_core_event(state: &AppState, kind: &str, entity_id: &str, seq: i64, summary: Value) {
    state.bus.publish(ServerEvent::VogtChanged {
        kind: kind.to_string(),
        entity_kind: "work_item".to_string(),
        entity_id: entity_id.to_string(),
        seq,
        summary,
    });
}

/// Poll a task until its run list has at least `n` runs, or time out.
pub async fn wait_for_run_count(
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
pub async fn stand_in_core_knowing(
    identities: Vec<(&'static str, &'static str, Vec<&'static str>)>,
) -> String {
    stand_in_core_relaying(identities, "").await.0
}

/// As `stand_in_core_knowing`, and its MCP `session_input` is carried out the
/// way the real core does it: the engine's input route, under `stack`, with
/// `person` saying whether the bearer that asked is a person. The engine's URL
/// goes in the returned slot once it has booted.
pub async fn stand_in_core_relaying(
    identities: Vec<(&'static str, &'static str, Vec<&'static str>)>,
    stack: &'static str,
) -> (String, Arc<std::sync::OnceLock<String>>) {
    use axum::{extract::State, http::HeaderMap, response::IntoResponse, routing::any, Router};
    type Known = Arc<Vec<(&'static str, &'static str, Vec<&'static str>)>>;
    type Relay = (Known, &'static str, Arc<std::sync::OnceLock<String>>);
    async fn mcp(
        (known, stack, engine): Relay,
        headers: HeaderMap,
        message: Value,
    ) -> axum::response::Response {
        let id = message.get("id").cloned().unwrap_or(json!(1));
        let result = match message["method"].as_str() {
            Some("tools/list") => json!({"tools": [{
                "name": "session_input",
                "description": "Type a line into a terminal session.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "id": {"type": "string"},
                        "text": {"type": "string"},
                        "submit": {"type": "boolean"},
                        "reason": {"type": "string"},
                    },
                    "required": ["id", "text", "reason"],
                    "additionalProperties": false,
                },
            }]}),
            Some("tools/call") => {
                let bearer = headers
                    .get(axum::http::header::AUTHORIZATION)
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.strip_prefix("Bearer "))
                    .unwrap_or_default();
                let person = known
                    .iter()
                    .any(|(token, who, _)| *token == bearer && !who.starts_with("agent:"));
                let args = &message["params"]["arguments"];
                let typed = reqwest::Client::new()
                    .post(format!(
                        "{}/api/sessions/{}/input",
                        engine.get().expect("engine booted"),
                        args["id"].as_str().unwrap_or_default()
                    ))
                    .bearer_auth(stack)
                    .json(&json!({
                        "text": args["text"],
                        "submit": args["submit"].as_bool().unwrap_or(false),
                        "person": person,
                    }))
                    .send()
                    .await
                    .unwrap();
                let refused = !typed.status().is_success();
                json!({
                    "content": [{"type": "text", "text": typed.text().await.unwrap()}],
                    "isError": refused,
                })
            }
            _ => json!({}),
        };
        axum::Json(json!({"jsonrpc": "2.0", "id": id, "result": result})).into_response()
    }
    async fn handler(
        State(relay): State<Relay>,
        headers: HeaderMap,
        uri: axum::http::Uri,
        body: axum::body::Bytes,
    ) -> axum::response::Response {
        if uri.path() == "/mcp" {
            let message = serde_json::from_slice(&body).unwrap_or(Value::Null);
            return mcp(relay, headers, message).await;
        }
        let known = relay.0;
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
                        // What the real core says of these: an `agent:`
                        // actor is an agent, anyone else a person.
                        "kind": if identity_ref.starts_with("agent:") { "agent" } else { "human" },
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
    let engine = Arc::new(std::sync::OnceLock::new());
    let app = Router::new().route("/{*path}", any(handler)).with_state((
        Arc::new(identities),
        stack,
        Arc::clone(&engine),
    ));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}"), engine)
}

pub fn auth() -> reqwest::header::HeaderMap {
    auth_for(TEST_TOKEN)
}

pub fn auth_for(token: &str) -> reqwest::header::HeaderMap {
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
pub fn fake_agent_path() -> String {
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
pub fn fake_agent_template() -> SessionTemplate {
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
        account: None,
        backend: "local".to_string(),
        host_capable: false,
        pod_capable: false,
    }
}

/// The command a run uses to invoke the fake-agent preset with a scenario.
pub fn fake_agent_command(scenario: &str) -> Vec<String> {
    vec![fake_agent_path(), scenario.to_string()]
}

/// Poll a task until its latest run leaves `running`, or time out.
pub async fn wait_for_run_finish(client: &reqwest::Client, base: &str, task_id: &str) -> Value {
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

/// Run a one-shot command in a new session and return what it printed once
/// it holds `expected`.
pub async fn session_output(base: &str, body: Value, expected: &str) -> String {
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
    session_output_after_exit(&client, base, &id, &[expected])
        .await
        .printed
}

/// Run a session to completion and return everything it printed.
/// A session that has exited, and everything it printed, once the scrollback
/// holds every string in `expected`.
///
/// The exit code and the PTY reader's last bytes race: on a loaded runner the
/// session can report its exit while the tail of the child's output is still
/// on its way into the scrollback. So this polls until both the exit and the
/// expected content are visible, and on its deadline returns what it has for
/// the caller's assertion to report.
pub struct ExitedSession {
    pub printed: String,
    pub exit_code: Option<i32>,
}

pub async fn session_output_after_exit(
    client: &reqwest::Client,
    base: &str,
    id: &str,
    expected: &[&str],
) -> ExitedSession {
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
            return ExitedSession {
                printed,
                exit_code: detail.summary.exit_code,
            };
        }
        if tokio::time::Instant::now() >= deadline {
            assert!(
                exited,
                "session {id} never exited with output; exit_code={:?}, scrollback_bytes={}",
                detail.summary.exit_code, detail.summary.scrollback_bytes,
            );
            return ExitedSession {
                printed,
                exit_code: detail.summary.exit_code,
            };
        }
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
}

pub async fn ws_attach(
    base: &str,
    id: &str,
) -> tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>> {
    ws_attach_with_token(base, id, TEST_TOKEN).await
}

pub async fn ws_attach_with_token(
    base: &str,
    id: &str,
    token: &str,
) -> tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>> {
    ws_attach_with_token_and_cursor(base, id, token, None).await
}

pub async fn ws_attach_with_token_and_cursor(
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
pub async fn ws_attach_with_auth(
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
pub const SNAPSHOT_HEAD_BYTES: usize = 64;

#[allow(dead_code)] // `first_bytes` is only read by the large-session tests.
pub async fn read_snapshot(
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
pub fn assert_ground_state_aligned(first_bytes: &[u8]) {
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
pub async fn fill_scrollback(base: &str, id: &str, at_least: usize) {
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
pub async fn kill_session(client: &reqwest::Client, base: &str, id: &str) {
    let _ = client
        .post(format!("{base}/api/sessions/{id}/kill"))
        .send()
        .await;
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
pub fn load_session_command(count: u32) -> Vec<String> {
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
pub async fn create_command_session(
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
pub const LOAD_DEADLINE: Duration = Duration::from_secs(45);

/// Read only `scrollback_pos`. `tail_bytes=1` keeps each poll from copying,
/// base64-encoding and JSON-parsing the whole multi-MiB ring every 20 ms,
/// which on a slow runner competed with the very producer being waited on.
pub async fn scrollback_pos(client: &reqwest::Client, base: &str, id: &str) -> u64 {
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
pub async fn poll_scrollback_at_least(
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
pub async fn wait_until_scrollback_stable(client: &reqwest::Client, base: &str, id: &str) -> u64 {
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
pub async fn read_snapshot_payload(
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

/// One delivered snapshot/resync sequence: whether it reset the terminal, its
/// payload (between `snapshot-start` and `snapshot-done`), and the live bytes
/// that followed it until the next `snapshot-start`.
pub struct SnapSegment {
    pub reset: bool,
    pub payload: Vec<u8>,
    pub live: Vec<u8>,
}

/// Drain frames, grouping the payload after each `snapshot-start` into its own
/// segment (the initial snapshot plus the live bytes after it, then each
/// in-band resync), until the client's own cursor reaches `target`. The cursor
/// is what a client tracks:
/// each `snapshot-start` sets it to that snapshot's `scrollback_pos`, and live
/// binary frames after `snapshot-done` advance it by their length. Waiting on
/// the cursor rather than an idle window means a slow runner cannot cut the
/// drain off before the in-band resync arrives. Fails on a deadline.
pub async fn read_segments_until_pos(
    ws: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    target: u64,
) -> Vec<SnapSegment> {
    let deadline = tokio::time::Instant::now() + LOAD_DEADLINE;
    let mut segs: Vec<SnapSegment> = Vec::new();
    let mut cursor = 0u64;
    let mut in_snapshot = false;
    while !(cursor >= target && !in_snapshot) {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let m = match tokio::time::timeout(remaining, ws.next()).await {
            Ok(Some(Ok(m))) => m,
            other => panic!(
                "stream ended before the client cursor reached {target} \
                 (cursor {cursor}, {} segment(s)): {other:?}",
                segs.len()
            ),
        };
        match m {
            Message::Text(s) => {
                let v: Value = serde_json::from_str(&s).unwrap();
                match v["type"].as_str() {
                    Some("snapshot-start") => {
                        in_snapshot = true;
                        cursor = v["scrollback_pos"].as_u64().unwrap();
                        segs.push(SnapSegment {
                            reset: v["reset"].as_bool().unwrap_or(true),
                            payload: Vec::new(),
                            live: Vec::new(),
                        });
                    }
                    Some("snapshot-done") => in_snapshot = false,
                    _ => {}
                }
            }
            Message::Binary(b) => {
                if let Some(seg) = segs.last_mut() {
                    if in_snapshot {
                        seg.payload.extend_from_slice(&b);
                    } else {
                        cursor += b.len() as u64;
                        seg.live.extend_from_slice(&b);
                    }
                }
            }
            _ => {}
        }
    }
    segs
}

/// Extract, in order, every complete `SEQ%08d` marker in `bytes`. The producer
/// emits each sequence number exactly once and monotonically, so this is the
/// spine used to detect gaps and duplicates in a replay.
pub fn seq_numbers(bytes: &[u8]) -> Vec<u32> {
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
pub fn assert_seq_spine_contiguous(bytes: &[u8]) {
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

pub async fn collect_binary_until(
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

// ── the activity state reaches the server-wide event stream ───────────────

/// Read the SSE stream until an event satisfies `want`, or give up.
///
/// SSE frames are `data: <json>` lines; keep-alive comments start with `:`
/// and are skipped. A chunk boundary can fall anywhere, so the partial line
/// is carried across reads rather than assumed to end on one.
pub async fn event_matching(
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

// ── the notifications that are worth a phone interruption ─────────────────

/// Deliveries a stand-in push service received, by the path they arrived on.
pub type PushLog = Arc<Mutex<Vec<String>>>;

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
pub async fn start_stand_in_push_service() -> (PushLog, String) {
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
pub fn web_push_keys() -> (String, String) {
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
pub fn admitting_only(kind: &str) -> Value {
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
pub async fn subscribe_for_kind(client: &reqwest::Client, base: &str, push_base: &str, kind: &str) {
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
pub async fn delivered_to(log: &PushLog, path: &str) {
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

pub fn nothing_delivered_to(log: &PushLog, path: &str) {
    let seen = log.lock().unwrap();
    assert!(
        !seen.iter().any(|got| got == path),
        "a notification was routed to {path}, which asked for a different \
         kind entirely; the service saw {seen:?}"
    );
}

// --- fail-closed approval gates and mid-run steering -----------------------

/// A config whose workspace and state dir are a throwaway tempdir. The dir must
/// outlive the server, so the caller holds the returned `TempDir`.
pub fn agent_task_config() -> (Config, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = test_config();
    cfg.default_cwd = tmp.path().to_path_buf();
    cfg.workspace_root = tmp.path().canonicalize().unwrap();
    cfg.state_dir = tmp.path().join("state");
    (cfg, tmp)
}

/// Create a task and start one run of it, returning `(task_id, run_id)`.
pub async fn create_and_run(client: &reqwest::Client, base: &str, body: Value) -> (String, String) {
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
pub async fn wait_for_gate<F>(client: &reqwest::Client, base: &str, task_id: &str, pred: F) -> Value
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
pub async fn session_scrollback(client: &reqwest::Client, base: &str, session_id: &str) -> String {
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

pub fn approve_hold_gate() -> Value {
    json!({
        "question": "Proceed with the deploy?",
        "options": [
            { "label": "Approve", "input": "go", "approve": true },
            { "label": "Hold", "input": "stop", "approve": false },
        ],
    })
}

// --- typed outcomes, the conclusion record, schema-validated findings -------

/// A committable git repo under `dir`, so a run's workspace has a branch and a
/// base sha for the conclusion to report against.
pub fn init_git_repo(dir: &std::path::Path) {
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

/// The head a git checkout is on, or `None` for one with no commit yet.
pub fn git_head(repo: &std::path::Path) -> Option<String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["rev-parse", "--verify", "--quiet", "HEAD"])
        .output()
        .unwrap();
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

pub fn findings_schema() -> Value {
    json!({
        "type": "object",
        "required": ["summary", "risk"],
        "properties": {
            "summary": { "type": "string" },
            "risk": { "type": "string", "enum": ["low", "medium", "high"] },
        },
    })
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
pub fn fake_broker_helper(dir: &std::path::Path) -> std::path::PathBuf {
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

pub fn broker_config(tmp: &tempfile::TempDir, manifest: &str) -> Config {
    let mut cfg = test_config();
    cfg.default_cwd = tmp.path().to_path_buf();
    cfg.workspace_root = tmp.path().canonicalize().unwrap();
    cfg.agent_auth_helper = fake_broker_helper(tmp.path());
    cfg.agent_auth_secrets = vogt_engine_server::secret_broker::parse_manifest(manifest).unwrap();
    cfg
}

pub async fn create_session_id(client: &reqwest::Client, base: &str, name: &str) -> String {
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

/// Poll the live list until `pred` holds for session `id`, returning its row.
pub async fn wait_for_session_row(
    client: &reqwest::Client,
    base: &str,
    id: &str,
    pred: impl Fn(&Value) -> bool,
) -> Value {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let list: Value = client
            .get(format!("{base}/api/sessions"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let list: Vec<Value> = serde_json::from_value(list["sessions"].clone()).unwrap();
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

pub async fn create_session_with(client: &reqwest::Client, base: &str, body: Value) -> String {
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

/// Like `event_matching`, but also returns the SSE `id:` of the matching frame.
pub async fn sse_event(
    stream: &mut (impl futures_util::Stream<Item = reqwest::Result<bytes::Bytes>> + Unpin),
    want: impl Fn(&Value) -> bool,
) -> (Value, Option<String>) {
    let mut partial = String::new();
    let mut frame_id: Option<String> = None;
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
                let line = line.trim_end();
                if let Some(id) = line.strip_prefix("id: ") {
                    frame_id = Some(id.to_string());
                    continue;
                }
                if line.is_empty() {
                    frame_id = None;
                    continue;
                }
                let Some(raw) = line.strip_prefix("data: ") else {
                    continue;
                };
                let Ok(event) = serde_json::from_str::<Value>(raw) else {
                    continue;
                };
                if want(&event) {
                    return (event, frame_id.clone());
                }
            }
        }
    })
    .await
    .expect("no matching event arrived on /api/events")
}

// ---------------------------------------------------------------------------
// Hibernation (WI-912)
// ---------------------------------------------------------------------------

/// The scrollback once it has grown past `len`, after a short settle so the
/// echo of the bytes that caused the growth is all present.
pub async fn wait_for_scrollback_growth(
    client: &reqwest::Client,
    base: &str,
    id: &str,
    len: usize,
) -> String {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let printed = session_scrollback(client, base, id).await;
        if printed.len() > len {
            tokio::time::sleep(Duration::from_millis(100)).await;
            return session_scrollback(client, base, id).await;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "session {id} printed nothing new; got {printed:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Poll a live session's scrollback until it holds every `expected` string.
pub async fn live_output_containing(
    client: &reqwest::Client,
    base: &str,
    id: &str,
    expected: &[&str],
) -> String {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        let printed = session_scrollback(client, base, id).await;
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
pub fn hibernation_sandbox() -> (tempfile::TempDir, Config, std::path::PathBuf) {
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

pub fn pid_alive(pid: i32) -> bool {
    // A zombie still answers kill(0); count it as gone.
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
    match stat.rfind(')') {
        Some(at) => !stat[at + 2..].starts_with('Z'),
        None => false,
    }
}

pub fn printed_value(printed: &str, key: &str) -> String {
    let start = printed.rfind(&format!("{key}=[")).unwrap() + key.len() + 2;
    let end = start + printed[start..].find(']').unwrap();
    printed[start..end].to_string()
}
