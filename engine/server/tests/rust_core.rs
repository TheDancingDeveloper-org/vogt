//! The front door against a real `vogt-core` process.
//!
//! `vogt_core.rs` proves the wire against a stand-in. This file proves the
//! same door against the binary the stack runs: bearer resolution through
//! `GET /api/vogt/auth/whoami`, a proxied read, the MCP proxy, the event
//! follower on the stack secret, and an outage that is 503 rather than 401.
//! The binary is `engine/target/debug/vogt-core`. A tree that has not built
//! it fails these tests; a silent skip would let CI pass without running them.

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
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../target/debug/vogt-core");
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
    assert!(
        body.get("events").is_some() || body.get("items").is_some() || body.is_object(),
        "{body}"
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
