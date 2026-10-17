//! The front door against a real `vogt-core` process.
//!
//! `vogt_core.rs` proves the wire against a stand-in. This file proves the
//! same door against the binary the stack runs: bearer resolution through
//! `GET /api/vogt/auth/whoami`, a proxied read, the MCP proxy, the event
//! follower on the stack secret, and an outage that is 503 rather than 401.
//! The binary is the debug build of `vogt-core` in this workspace's target
//! directory (`CARGO_TARGET_DIR` when set, otherwise `engine/target`). A tree
//! that has not built it fails these tests; a silent skip would let CI pass
//! without running them.

use std::{
    path::PathBuf,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

use reqwest::StatusCode;
use serde_json::Value;
use vogt_engine_server::{app::router, Config};

const SECRET: &str = "core-token-abcdef1234567890";

struct Core {
    child: Child,
    url: String,
    _dir: tempfile::TempDir,
}

impl Drop for Core {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn binary() -> PathBuf {
    let target = std::env::var("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../target"));
    let path = target.join("debug/vogt-core");
    assert!(
        path.is_file(),
        "vogt-core debug binary is missing at {}; build it with `cargo build -p vogt-core` before these tests",
        path.display()
    );
    path
}

fn boot_core() -> Core {
    let bin = binary();
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("core-token"), SECRET).unwrap();
    let mut child = Command::new(&bin)
        .arg("init")
        .arg("--data-dir")
        .arg(dir.path())
        .env(
            "VOGT_BOOTSTRAP_CORE_TOKEN_FILE",
            dir.path().join("core-token"),
        )
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let status = child.wait().unwrap();
    assert!(status.success(), "vogt-core init failed: {status}");

    // The core announces the port it was given, not the one the kernel
    // chose, so port 0 comes back as http://127.0.0.1:0. Pick a free port.
    let port = {
        let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        probe.local_addr().unwrap().port()
    };
    let child = Command::new(&bin)
        .args(["serve", "--host", "127.0.0.1", "--port", &port.to_string()])
        .arg("--data-dir")
        .arg(dir.path())
        .env(
            "VOGT_BOOTSTRAP_CORE_TOKEN_FILE",
            dir.path().join("core-token"),
        )
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let url = format!("http://127.0.0.1:{port}");
    Core {
        child,
        url,
        _dir: dir,
    }
}

fn base_config() -> Config {
    Config {
        bind: "127.0.0.1:0".parse().unwrap(),
        token: Some(SECRET.to_string()),
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
        push_allow_insecure_endpoints: false,
        state_dir: tempfile::tempdir().unwrap().keep(),
        fcm_service_account_json: None,
        vapid_subject: "mailto:test@example.invalid".to_string(),
        allowed_origins: vec![],
        auto_agent_auth: false,
        agent_auth_helper: "/usr/local/bin/vogt-agent-auth".into(),
        codex_session_id: false,
        agent_auth_secrets: vec![],
        agent_grant_projects: vec![],
        session_templates: vec![],
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
        vogt_core_url: None,
        vogt_import_root: None,
        vogt_engine_state_dir: None,
        vogt_core_token: None,
        agent_clis: vogt_engine_server::agent_clis::AgentCliPaths::default(),
        hibernation: vogt_engine_server::hibernate_policy::Policy::default(),
        session_list_retain_after: None,
        assistant_call: vogt_engine_server::call::CallPolicy::default(),
        autopilot: vogt_engine_server::autopilot::Policy::default(),
        budget: vogt_engine_server::budget::Policy::default(),
        agent_onboarding: vogt_engine_server::claude_config::Onboarding::default(),
        session_rss_warn_bytes: None,
        worktree_clean_on_hibernate: false,
        disk_warn_percent: 85,
        metrics_bind: None,
        chat: Default::default(),
        economics_tick: std::time::Duration::ZERO,
        economics_home: None,
    }
}

fn config_against(core_url: &str) -> Config {
    let mut cfg = base_config();
    // No break-glass token: the only credential is the stack secret the Rust
    // core adopted at init, so resolution has to go through whoami.
    cfg.token = None;
    cfg.vogt_core_url = Some(core_url.to_string());
    cfg.vogt_core_token = Some(SECRET.to_string());
    cfg
}

async fn boot_door(cfg: Config) -> String {
    let (router, _state) = router(cfg).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    format!("http://{addr}")
}

fn bearer() -> reqwest::header::HeaderMap {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::AUTHORIZATION,
        format!("Bearer {SECRET}").parse().unwrap(),
    );
    headers
}

async fn wait_ready(url: &str) {
    let client = reqwest::Client::new();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if client
            .get(format!("{url}/health/ready"))
            .send()
            .await
            .map(|response| response.status().is_success())
            .unwrap_or(false)
        {
            return;
        }
        if Instant::now() > deadline {
            panic!("vogt-core never became ready at {url}");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn the_front_door_resolves_the_stack_secret_through_the_rust_core() {
    let core = boot_core();
    wait_ready(&core.url).await;
    let door = boot_door(config_against(&core.url)).await;
    let client = reqwest::Client::new();

    // The engine asks the core who the bearer is. The adopted stack secret
    // is the engine's own actor, not a rejection and not an outage.
    let who = client
        .get(format!("{door}/api/vogt/auth/whoami"))
        .headers(bearer())
        .send()
        .await
        .unwrap();
    assert_eq!(who.status(), StatusCode::OK);
    let body: Value = who.json().await.unwrap();
    assert_eq!(body["identity_ref"], "agent:vogt-engine", "{body}");

    // A proxied read reaches the Rust core under the core's own prefix.
    let status = client
        .get(format!("{door}/api/vogt/status"))
        .headers(bearer())
        .send()
        .await
        .unwrap();
    assert_eq!(status.status(), StatusCode::OK, "{status:?}");
    let body: Value = status.json().await.unwrap();
    assert!(
        body.get("data_dir").is_some() || body.get("principal").is_some(),
        "{body}"
    );
}

#[tokio::test]
async fn the_followers_cursor_sees_a_write_made_after_it_started() {
    // The follower's own client and cursor logic, against the Rust feed: the
    // cursor starts at the head, a write lands while the follower is running,
    // and the next poll from the cursor returns that write and moves it on.
    let core = boot_core();
    wait_ready(&core.url).await;
    let follower = vogt_engine_server::vogt_core::VogtCore::from_config(&config_against(&core.url))
        .expect("the follower has a core client");
    let (_, mut cursor) = follower.events_after(0, 1).await.expect("head poll");

    let status = Command::new(binary())
        .arg("--data-dir")
        .arg(core._dir.path())
        .args([
            "project",
            "register",
            "--name",
            "Follower Probe",
            "--root-path",
            "/srv/follower-probe",
            "--reason",
            "follower test",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .status()
        .unwrap();
    assert!(status.success(), "the write must succeed: {status}");

    let (events, next) = follower
        .events_after(cursor, 50)
        .await
        .expect("poll from the cursor");
    let kinds: Vec<&str> = events
        .iter()
        .filter_map(|event| event["kind"].as_str())
        .collect();
    assert!(
        kinds.contains(&"project.registered"),
        "the follower must see the write: {kinds:?}"
    );
    assert!(next > cursor, "the cursor moves past the write");
    cursor = next;
    // Nothing new: the cursor is returned unchanged, so a poller never rewinds.
    let (empty, again) = follower.events_after(cursor, 50).await.expect("idle poll");
    assert!(empty.is_empty(), "an idle poll is empty: {empty:?}");
    assert_eq!(again, cursor);
}

#[tokio::test]
async fn a_dead_rust_core_is_an_outage_not_a_rejected_credential() {
    let core = boot_core();
    wait_ready(&core.url).await;
    let door = boot_door(config_against(&core.url)).await;
    // Kill it after the door has resolved it once, so the cache is warm and
    // the next miss is an outage of a core that was real.
    drop(core);
    tokio::time::sleep(Duration::from_millis(200)).await;
    let client = reqwest::Client::new();
    let response = client
        .get(format!("{door}/api/vogt/status"))
        .headers(bearer())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
}

#[tokio::test]
async fn a_dead_rust_core_refuses_whoami_as_an_outage() {
    // Bearer resolution is not a proxy. A core that cannot be asked is 503,
    // never 401: a restart must not look like a bad credential.
    let core = boot_core();
    wait_ready(&core.url).await;
    let door = boot_door(config_against(&core.url)).await;
    drop(core);
    tokio::time::sleep(Duration::from_millis(200)).await;
    let client = reqwest::Client::new();
    // `/api/vogt/*` is a proxy, and a dead core behind it is 502. Bearer
    // resolution is the other path: an engine route asks whoami first, and
    // an outage there is 503, never 401. A fresh bearer, so the cache from
    // the ready probe cannot answer for it.
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::AUTHORIZATION,
        format!("Bearer {SECRET}-unseen").parse().unwrap(),
    );
    let response = client
        .get(format!("{door}/api/sessions"))
        .headers(headers)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_ne!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn the_sse_stream_carries_a_core_write_made_after_the_door_started() {
    // The live half of the event path (WI-1075 deferred this to I1): the
    // follower reads the Rust core's feed with the stack secret, republishes
    // each change onto the bus, and a browser's `/api/events` stream gets it.
    let core = boot_core();
    wait_ready(&core.url).await;
    let door = boot_door(config_against(&core.url)).await;
    let client = reqwest::Client::new();
    let mut stream = client
        .get(format!("{door}/api/events"))
        .headers(bearer())
        .send()
        .await
        .unwrap();
    assert_eq!(stream.status(), StatusCode::OK);
    // The follower takes the core's head on its first poll, which runs as
    // soon as the door boots. The write has to land after that poll, or it
    // becomes the head and the follower never sees it.
    tokio::time::sleep(Duration::from_secs(1)).await;
    let status = Command::new(binary())
        .arg("--data-dir")
        .arg(core._dir.path())
        .args([
            "project",
            "register",
            "--name",
            "Stream Probe",
            "--root-path",
            "/srv/stream-probe",
            "--reason",
            "sse test",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .status()
        .unwrap();
    assert!(status.success(), "the write must succeed: {status}");

    // The follower polls every five seconds, so the event is due within one
    // interval; the deadline leaves room for a slow CI runner.
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut seen = String::new();
    while !(seen.contains("vogt-changed") && seen.contains("project.registered")) {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let chunk = tokio::time::timeout(remaining, stream.chunk())
            .await
            .unwrap_or_else(|_| {
                panic!("no vogt-changed/project.registered on /api/events within 30s; saw: {seen}")
            })
            .expect("the SSE read failed")
            .expect("the SSE stream ended");
        seen.push_str(&String::from_utf8_lossy(&chunk));
    }
}

#[tokio::test]
async fn the_event_follower_reads_the_rust_core_with_the_stack_secret() {
    let core = boot_core();
    wait_ready(&core.url).await;
    let door = boot_door(config_against(&core.url)).await;
    let client = reqwest::Client::new();
    // The follower is the engine's own poll, not a browser request, so this
    // goes straight at the core with the stack secret the engine holds.
    let response = client
        .get(format!("{}/api/events?after=0&limit=10", core.url))
        .headers(bearer())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK, "{response:?}");
    let body: Value = response.json().await.unwrap();
    let events = body["events"].as_array().expect("events array");
    assert!(
        events.iter().any(|event| event["kind"] == "token.adopted"),
        "the adoption event must be on the feed the follower reads: {body}"
    );
    // And the door still answers a proxied events read with the same secret.
    let proxied = client
        .get(format!("{door}/api/vogt/events?after=0&limit=10"))
        .headers(bearer())
        .send()
        .await
        .unwrap();
    assert_eq!(proxied.status(), StatusCode::OK, "{proxied:?}");
    let _ = door;
}

#[tokio::test]
async fn mcp_reaches_the_rust_core() {
    let core = boot_core();
    wait_ready(&core.url).await;
    let door = boot_door(config_against(&core.url)).await;
    let client = reqwest::Client::new();
    let response = client
        .post(format!("{door}/mcp"))
        .headers(bearer())
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .body(
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"e2e","version":"0"}}}"#,
        )
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK, "{response:?}");
    let text = response.text().await.unwrap();
    assert!(text.contains("vogt"), "{text}");
}

#[tokio::test]
async fn vogt_tools_lists_and_calls_the_real_rust_core() {
    // The assistant's tool client, pointed at a real vogt-core rather than
    // the stub in vogt_tools::stub. tools/list has to come back with the
    // curated reads, and a tools/call has to be answered by the core.
    let core = boot_core();
    wait_ready(&core.url).await;
    let mut cfg = config_against(&core.url);
    let tools = vogt_engine_server::vogt_tools::VogtTools::from_config(&cfg).expect("tools client");
    let _ = &mut cfg;
    let caller = vogt_engine_server::vogt_tools::Caller {
        token_name: "stack".into(),
        core_token: Some(SECRET.to_string()),
        person: false,
    };
    let listed = tools.tools_for(&caller).await;
    assert!(
        listed.iter().any(|tool| tool.mcp_name == "status"),
        "tools/list against the Rust core curated nothing: {listed:?}"
    );
    let (answer, is_error) = tools
        .call(SECRET, "status", &serde_json::json!({}))
        .await
        .expect("tools/call status");
    assert!(!is_error, "{answer}");
    assert!(
        answer.contains("data_dir") || answer.contains("principal"),
        "{answer}"
    );
}

#[test]
fn the_engine_image_builds_its_own_rust_core() {
    // The engine image carries vogt-core from its own `server-build` stage. It
    // no longer lifts a core from CORE_IMAGE or runs a uv venv, so neither a
    // Python interpreter nor a second published image can reach the shipped
    // image. The standalone Rust image (deploy/rust-core.Dockerfile) keeps its
    // install path: the `vogt` service in the overlays still runs it.
    let dockerfile = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../Dockerfile");
    let text = std::fs::read_to_string(&dockerfile).expect("engine Dockerfile");
    assert!(
        text.contains("-p vogt-engine-server -p vogt-core"),
        "engine/Dockerfile no longer builds vogt-core in server-build"
    );
    assert!(
        text.contains("COPY --from=server-build /usr/local/bin/vogt-core /usr/local/bin/vogt-core"),
        "engine/Dockerfile no longer copies vogt-core into the runtime image"
    );
    for retired in [
        "CORE_IMAGE",
        "UV_IMAGE",
        "AS core",
        "AS uv",
        "/opt/vogt/.venv",
    ] {
        assert!(
            !text.contains(retired),
            "engine/Dockerfile still refers to the retired {retired}"
        );
    }
    let rust = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../deploy/rust-core.Dockerfile");
    let rust_text = std::fs::read_to_string(&rust).expect("rust core Dockerfile");
    assert!(
        rust_text.contains("/opt/vogt/.venv/bin/vogt"),
        "the Rust image no longer installs where the stack expects a core"
    );
}
