//! End-to-end proof of the server-side speech proxy (FR-T12), against a
//! stand-in OpenAI-compatible audio server rather than a mocked client — the
//! only way to assert what actually crosses the wire: that the engine forwards
//! a real multipart body to `/audio/transcriptions`, POSTs the right JSON to
//! `/audio/speech`, iterates an ordered base-URL list on failure, bounds each
//! attempt, and 404s (never 500s) when every entry is unreachable so a client
//! falls back (FR-T6). Audio is proxied and never persisted.

use std::{
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use axum::{
    extract::{Multipart, State},
    http::{header, StatusCode as AxumStatus},
    response::IntoResponse,
    routing::post,
    Json, Router,
};
use reqwest::StatusCode;
use serde_json::{json, Value};
use vogt_engine_server::{app::router, Config};

const TEST_TOKEN: &str = "test-token-1234567890abcdef";
/// The exact bytes the mock TTS server "synthesises"; the test asserts the
/// engine streams these back unchanged.
const MOCK_AUDIO: &[u8] = b"ID3\x03fake-mp3-bytes-\x00\x01\x02\x03";

/// What the mock audio server saw, so the test asserts the engine's upstream
/// HTTP rather than trusting the reply it fabricated.
#[derive(Debug, Default)]
struct MockSeen {
    /// The STT request carried a multipart `file` part.
    stt_saw_file: bool,
    /// The `model` form field the engine sent to `/audio/transcriptions`.
    stt_model: Option<String>,
    /// The JSON body the engine POSTed to `/audio/speech`.
    tts_body: Option<Value>,
}

type MockLog = Arc<Mutex<MockSeen>>;

async fn mock_transcriptions(State(log): State<MockLog>, mut mp: Multipart) -> impl IntoResponse {
    let mut saw_file = false;
    let mut model = None;
    while let Some(field) = mp.next_field().await.unwrap() {
        let name = field.name().map(str::to_owned);
        if field.file_name().is_some() {
            // The audio part. Drain it so the body is fully read.
            let bytes = field.bytes().await.unwrap();
            if name.as_deref() == Some("file") && !bytes.is_empty() {
                saw_file = true;
            }
        } else if name.as_deref() == Some("model") {
            model = Some(field.text().await.unwrap());
        }
    }
    {
        let mut seen = log.lock().unwrap();
        seen.stt_saw_file = saw_file;
        seen.stt_model = model;
    }
    Json(json!({ "text": "the transcript from the mock" }))
}

async fn mock_speech(State(log): State<MockLog>, Json(body): Json<Value>) -> impl IntoResponse {
    log.lock().unwrap().tts_body = Some(body);
    ([(header::CONTENT_TYPE, "audio/mpeg")], MOCK_AUDIO.to_vec())
}

/// A slow endpoint that outlasts the per-attempt timeout, to prove the bound
/// moves the request on rather than hanging it.
async fn mock_hang() -> impl IntoResponse {
    tokio::time::sleep(Duration::from_secs(30)).await;
    (AxumStatus::OK, "too late").into_response()
}

/// Bring up the mock audio server; returns its base URL and the log.
async fn mock_audio_server() -> (String, MockLog) {
    let log: MockLog = Arc::new(Mutex::new(MockSeen::default()));
    let app = Router::new()
        .route("/audio/transcriptions", post(mock_transcriptions))
        .route("/audio/speech", post(mock_speech))
        .route("/hang/audio/transcriptions", post(mock_hang))
        .route("/hang/audio/speech", post(mock_hang))
        .with_state(Arc::clone(&log));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr: SocketAddr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    (format!("http://{addr}"), log)
}

/// An address nothing listens on — bind to claim a free port, then drop it, so
/// a connection is refused immediately (the "dead endpoint" case).
async fn dead_url() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    format!("http://{addr}")
}

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
        agent_auth_helper: "/usr/local/bin/mydevenv2-agent-auth".into(),
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
        // Short per-attempt bound so the "hanging endpoint" cases resolve fast.
        assistant_speech_attempt_timeout_ms: 300,
        public_url: None,
        vogt_core_url: None,
        vogt_import_root: None,
        vogt_engine_state_dir: None,
        vogt_core_token: None,
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
async fn stt_forwards_multipart_and_returns_the_transcript() {
    let (mock, log) = mock_audio_server().await;
    let mut cfg = test_config();
    cfg.assistant_stt_base_urls = vec![mock];
    // No key: a local, keyless endpoint is a valid entry.
    let base = boot(cfg).await;

    let form = reqwest::multipart::Form::new().part(
        "file",
        reqwest::multipart::Part::bytes(b"\x1aE\xdf\xa3-webm-audio".to_vec())
            .file_name("take.webm")
            .mime_str("audio/webm")
            .unwrap(),
    );
    let res = auth_client()
        .post(format!("{base}/api/assistant/stt"))
        .multipart(form)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body: Value = res.json().await.unwrap();
    assert_eq!(body["text"], "the transcript from the mock");

    // The engine actually sent multipart with a file part and the model field —
    // it did not fabricate the transcript.
    let seen = log.lock().unwrap();
    assert!(
        seen.stt_saw_file,
        "engine must forward a multipart file part"
    );
    assert_eq!(seen.stt_model.as_deref(), Some("whisper-1"));
}

#[tokio::test]
async fn tts_posts_json_and_streams_the_audio_back() {
    let (mock, log) = mock_audio_server().await;
    let mut cfg = test_config();
    cfg.assistant_tts_base_urls = vec![mock];
    let base = boot(cfg).await;

    let res = auth_client()
        .post(format!("{base}/api/assistant/tts"))
        .json(&json!({ "text": "read me the backlog" }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let content_type = res
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    assert!(content_type.starts_with("audio/"), "got {content_type:?}");
    let bytes = res.bytes().await.unwrap();
    assert_eq!(
        bytes.as_ref(),
        MOCK_AUDIO,
        "engine must stream the mock's bytes unchanged"
    );

    // The engine POSTed the OpenAI `/audio/speech` body shape.
    let seen = log.lock().unwrap();
    let sent = seen.tts_body.as_ref().expect("tts body");
    assert_eq!(sent["model"], "tts-1-hd");
    assert_eq!(sent["input"], "read me the backlog");
    assert_eq!(sent["voice"], "nova");
}

#[tokio::test]
async fn stt_fails_over_from_a_dead_first_endpoint_to_the_live_one() {
    let (mock, log) = mock_audio_server().await;
    let dead = dead_url().await;
    let mut cfg = test_config();
    // Local-first, cloud-fallback shape: the first entry refuses, the second
    // answers. The engine must iterate transparently.
    cfg.assistant_stt_base_urls = vec![dead, mock];
    let base = boot(cfg).await;

    let form = reqwest::multipart::Form::new().part(
        "file",
        reqwest::multipart::Part::bytes(b"audio".to_vec())
            .file_name("take.webm")
            .mime_str("audio/webm")
            .unwrap(),
    );
    let res = auth_client()
        .post(format!("{base}/api/assistant/stt"))
        .multipart(form)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(
        res.json::<Value>().await.unwrap()["text"],
        "the transcript from the mock"
    );
    assert!(
        log.lock().unwrap().stt_saw_file,
        "the live fallback was reached"
    );
}

#[tokio::test]
async fn all_endpoints_dead_yields_404_not_500() {
    let dead = dead_url().await;
    let mut cfg = test_config();
    cfg.assistant_stt_base_urls = vec![dead.clone()];
    cfg.assistant_tts_base_urls = vec![dead];
    let base = boot(cfg).await;

    let form = reqwest::multipart::Form::new().part(
        "file",
        reqwest::multipart::Part::bytes(b"audio".to_vec())
            .file_name("take.webm")
            .mime_str("audio/webm")
            .unwrap(),
    );
    let stt = auth_client()
        .post(format!("{base}/api/assistant/stt"))
        .multipart(form)
        .send()
        .await
        .unwrap();
    // 404 so the client falls back (FR-T6) — never 500.
    assert_eq!(stt.status(), StatusCode::NOT_FOUND);

    let tts = auth_client()
        .post(format!("{base}/api/assistant/tts"))
        .json(&json!({ "text": "hello" }))
        .send()
        .await
        .unwrap();
    assert_eq!(tts.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_hanging_first_endpoint_times_out_and_moves_on() {
    let (mock, _log) = mock_audio_server().await;
    let mut cfg = test_config();
    // The per-attempt timeout is 300ms; the hang route sleeps 30s. The request
    // must not wait for the hang — it must give up and reach the live entry.
    cfg.assistant_tts_base_urls = vec![format!("{mock}/hang"), mock.clone()];
    let base = boot(cfg).await;

    let started = Instant::now();
    let res = auth_client()
        .post(format!("{base}/api/assistant/tts"))
        .json(&json!({ "text": "hello" }))
        .send()
        .await
        .unwrap();
    let elapsed = started.elapsed();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.bytes().await.unwrap().as_ref(), MOCK_AUDIO);
    // Well under the 30s hang: the per-attempt bound moved it on.
    assert!(
        elapsed < Duration::from_secs(5),
        "the hang should have timed out fast, took {elapsed:?}"
    );
}
