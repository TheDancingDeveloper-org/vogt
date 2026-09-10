//! `POST /api/client-log`: the client-diagnostics ingest is bounded and auth'd.

use std::time::Duration;

use reqwest::StatusCode;
use serde_json::json;
use vogt_engine_server::{app::router, Config};

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
        ws_query_token_allowed: false,
        push_allow_insecure_endpoints: false,
        state_dir: tempfile::tempdir().unwrap().keep(),
        fcm_service_account_json: None,
        vapid_subject: "mailto:test@example.invalid".to_string(),
        allowed_origins: vec![],
        auto_agent_auth: false,
        agent_auth_helper: "/usr/local/bin/vogt-agent-auth".into(),
        agent_auth_secrets: vec![],
        session_templates: vec![],
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
        assistant_tts_base_urls: vec![],
        assistant_tts_api_key: None,
        assistant_tts_model: "tts-1-hd".into(),
        assistant_tts_voice: "nova".into(),
        assistant_tts_format: "mp3".into(),
        assistant_speech_attempt_timeout_ms: 300,
        public_url: None,
        vogt_core_url: None,
        vogt_import_root: None,
        vogt_engine_state_dir: None,
        vogt_core_token: None,
        agent_clis: vogt_engine_server::agent_clis::AgentCliPaths::default(),
    }
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

fn auth_client() -> reqwest::Client {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::AUTHORIZATION,
        format!("Bearer {TEST_TOKEN}").parse().unwrap(),
    );
    reqwest::Client::builder()
        .default_headers(headers)
        .build()
        .unwrap()
}

#[tokio::test]
async fn client_log_accepts_a_bounded_batch() {
    let base = boot(test_config()).await;
    let res = auth_client()
        .post(format!("{base}/api/client-log"))
        .json(&json!({ "events": [
            { "t": 1, "event": "tts.blob", "fields": { "type": "audio/wav", "size": 1234 } },
            { "t": 2, "event": "tts.play.rejected", "fields": { "name": "NotSupportedError" } }
        ]}))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn client_log_rejects_an_oversized_batch_and_bad_names() {
    let base = boot(test_config()).await;
    let client = auth_client();
    let too_many: Vec<_> = (0..65)
        .map(|i| json!({ "t": i, "event": "e", "fields": {} }))
        .collect();
    let res = client
        .post(format!("{base}/api/client-log"))
        .json(&json!({ "events": too_many }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    let res = client
        .post(format!("{base}/api/client-log"))
        .json(&json!({ "events": [{ "t": 1, "event": "not a name", "fields": {} }] }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    let res = client
        .post(format!("{base}/api/client-log"))
        .json(&json!({ "events": [] }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn client_log_requires_a_token() {
    let base = boot(test_config()).await;
    let res = reqwest::Client::new()
        .post(format!("{base}/api/client-log"))
        .json(&json!({ "events": [{ "t": 1, "event": "x", "fields": {} }] }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}
