//! End-to-end proof of the live call (`/api/assistant/call`, WI-960), over a
//! real WebSocket against stand-in OpenAI-compatible chat and audio servers:
//! a spoken turn is endpointed, transcribed, answered as a stream and spoken
//! back a piece at a time; speaking over the reply stops it and keeps only
//! what was heard; and a spoken "yes" never approves a change.

use std::{
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::Duration,
};

use axum::{
    extract::State,
    http::header,
    response::{IntoResponse, Response},
    routing::post,
    Json, Router,
};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::Message;
use vogt_engine_server::{app::router, Config};

const TEST_TOKEN: &str = "test-token-1234567890abcdef";
const RATE: u32 = 16_000;

// ----- Stand-ins -------------------------------------------------------------

#[derive(Default)]
struct Stubs {
    /// What the next transcription says.
    transcript: String,
    /// Scripted chat replies, in order: each a `choices[0].message`.
    chat: Vec<Value>,
    chat_calls: usize,
    /// Delay before each synthesized clip, to keep a reply "speaking".
    tts_delay_ms: u64,
    tts_inputs: Vec<String>,
}

type Shared = Arc<Mutex<Stubs>>;

fn wav(ms: u32) -> Vec<u8> {
    let samples = (24_000 * ms / 1000) as usize;
    let data = (samples * 2) as u32;
    let mut out = Vec::new();
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&24_000u32.to_le_bytes());
    out.extend_from_slice(&48_000u32.to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data.to_le_bytes());
    out.resize(out.len() + data as usize, 0);
    out
}

async fn chat(State(stubs): State<Shared>, Json(body): Json<Value>) -> Response {
    let message = {
        let mut stubs = stubs.lock().unwrap();
        stubs.chat_calls += 1;
        if stubs.chat.is_empty() {
            json!({"role": "assistant", "content": "script exhausted"})
        } else {
            stubs.chat.remove(0)
        }
    };
    if body.get("stream") != Some(&json!(true)) {
        return Json(json!({"choices": [{"message": message}]})).into_response();
    }
    let mut sse = String::new();
    if let Some(calls) = message.get("tool_calls").and_then(Value::as_array) {
        for (index, call) in calls.iter().enumerate() {
            let mut call = call.clone();
            call["index"] = json!(index);
            sse.push_str(&format!(
                "data: {}\n\n",
                json!({"choices": [{"delta": {"tool_calls": [call]}}]})
            ));
        }
    }
    if let Some(text) = message.get("content").and_then(Value::as_str) {
        for word in text.split_inclusive(' ') {
            sse.push_str(&format!(
                "data: {}\n\n",
                json!({"choices": [{"delta": {"content": word}}]})
            ));
        }
    }
    sse.push_str("data: [DONE]\n\n");
    ([(header::CONTENT_TYPE, "text/event-stream")], sse).into_response()
}

async fn transcriptions(
    State(stubs): State<Shared>,
    mut multipart: axum::extract::Multipart,
) -> impl IntoResponse {
    while let Ok(Some(field)) = multipart.next_field().await {
        let _ = field.bytes().await;
    }
    let text = stubs.lock().unwrap().transcript.clone();
    Json(json!({ "text": text }))
}

async fn speech(State(stubs): State<Shared>, Json(body): Json<Value>) -> impl IntoResponse {
    let delay = {
        let mut stubs = stubs.lock().unwrap();
        stubs
            .tts_inputs
            .push(body["input"].as_str().unwrap_or_default().to_string());
        stubs.tts_delay_ms
    };
    tokio::time::sleep(Duration::from_millis(delay)).await;
    ([(header::CONTENT_TYPE, "audio/wav")], wav(400))
}

async fn stub_server(stubs: Shared) -> String {
    let app = Router::new()
        .route("/v1/chat/completions", post(chat))
        .route("/v1/audio/transcriptions", post(transcriptions))
        .route("/v1/audio/speech", post(speech))
        .with_state(stubs);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr: SocketAddr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{addr}/v1")
}

fn test_config(stub: &str) -> Config {
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
        push_allow_insecure_endpoints: false,
        state_dir: tempfile::tempdir().unwrap().keep(),
        fcm_service_account_json: None,
        vapid_subject: "mailto:test@example.invalid".to_string(),
        allowed_origins: vec![],
        auto_agent_auth: false,
        agent_auth_helper: "/usr/local/bin/vogt-agent-auth".into(),
        agent_auth_secrets: vec![],
        agent_grant_projects: vec![],
        session_templates: vec![],
        assistant_api_key: Some("sk-test".into()),
        assistant_base_url: stub.to_string(),
        assistant_model: "test-model".into(),
        assistant_max_tool_calls: 8,
        assistant_allow_claude_proxy: false,
        assistant_reasoning_effort: None,
        assistant_profiles: vec![],
        assistant_default_profile: None,
        assistant_log_retention_days: 30,
        history_retention_days: 30,
        history_live_scan_bytes: 256 * 1024,
        assistant_stt_base_urls: vec![stub.to_string()],
        assistant_stt_api_key: None,
        assistant_stt_model: "whisper-1".into(),
        assistant_stt_language: "en".into(),
        assistant_tts_base_urls: vec![stub.to_string()],
        assistant_tts_api_key: None,
        assistant_tts_model: "tts-1".into(),
        assistant_tts_voice: "alloy".into(),
        assistant_tts_format: "wav".into(),
        assistant_speech_attempt_timeout_ms: 5_000,
        public_url: None,
        vogt_core_url: None,
        vogt_import_root: None,
        vogt_engine_state_dir: None,
        vogt_core_token: None,
        agent_clis: vogt_engine_server::agent_clis::AgentCliPaths::default(),
        hibernation: vogt_engine_server::hibernate_policy::Policy::default(),
        assistant_call: vogt_engine_server::call::CallPolicy {
            // Captions re-transcribe on a timer; off here so each turn makes
            // exactly the transcription calls the test expects.
            partial_interval_ms: 0,
            ..vogt_engine_server::call::CallPolicy::default()
        },
        autopilot: vogt_engine_server::autopilot::Policy::default(),
        agent_onboarding: vogt_engine_server::claude_config::Onboarding::default(),
        session_rss_warn_bytes: None,
        metrics_bind: None,
    }
}

async fn boot(cfg: Config) -> String {
    let (router, _state) = router(cfg).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    format!("127.0.0.1:{}", addr.port())
}

fn client() -> reqwest::Client {
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

// ----- Audio -----------------------------------------------------------------

fn tone(ms: u32, amplitude: f32) -> Vec<i16> {
    let n = (RATE * ms / 1000) as usize;
    (0..n)
        .map(|i| {
            let t = i as f32 / RATE as f32;
            (amplitude * 32767.0 * (2.0 * std::f32::consts::PI * 220.0 * t).sin()) as i16
        })
        .collect()
}

fn quiet(ms: u32) -> Vec<i16> {
    let n = (RATE * ms / 1000) as usize;
    (0..n).map(|i| ((i * 7919) % 41) as i16 - 20).collect()
}

/// A spoken turn: a beat of room sound, voice, and the silence that ends it.
fn utterance() -> Vec<i16> {
    let mut audio = quiet(300);
    audio.extend(tone(900, 0.3));
    audio.extend(quiet(1_000));
    audio
}

// ----- The call client -------------------------------------------------------

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn dial(addr: &str, token: &str) -> Ws {
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/api/assistant/call"))
        .await
        .unwrap();
    ws.send(Message::Text(
        json!({"type": "auth", "token": token}).to_string().into(),
    ))
    .await
    .unwrap();
    ws
}

async fn send(ws: &mut Ws, event: Value) {
    ws.send(Message::Text(event.to_string().into()))
        .await
        .unwrap();
}

async fn speak(ws: &mut Ws, audio: &[i16]) {
    for frame in audio.chunks(320) {
        let bytes: Vec<u8> = frame.iter().flat_map(|s| s.to_le_bytes()).collect();
        ws.send(Message::Binary(bytes.into())).await.unwrap();
    }
}

/// Everything the server sent, as events; a binary frame is recorded as
/// `{"type":"<binary>","bytes":n}` in its place.
async fn read_until(ws: &mut Ws, seen: &mut Vec<Value>, stop: impl Fn(&Value) -> bool) -> Value {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        let message = tokio::time::timeout_at(deadline, ws.next())
            .await
            .unwrap_or_else(|_| panic!("timed out; saw {seen:#?}"))
            .expect("socket open")
            .expect("frame");
        let event = match message {
            Message::Text(text) => serde_json::from_str::<Value>(&text).unwrap(),
            Message::Binary(bytes) => json!({"type": "<binary>", "bytes": bytes.len()}),
            Message::Close(frame) => json!({"type": "<close>", "frame": format!("{frame:?}")}),
            _ => continue,
        };
        seen.push(event.clone());
        if stop(&event) {
            return event;
        }
    }
}

fn is(kind: &'static str) -> impl Fn(&Value) -> bool {
    move |event| event["type"] == kind
}

fn types(seen: &[Value]) -> Vec<String> {
    seen.iter()
        .map(|e| e["type"].as_str().unwrap_or_default().to_string())
        .collect()
}

fn final_reply(text: &str) -> Value {
    json!({"role": "assistant", "content": text})
}

async fn setup(stubs: Stubs) -> (String, Shared) {
    let stubs = Arc::new(Mutex::new(stubs));
    let stub = stub_server(Arc::clone(&stubs)).await;
    let addr = boot(test_config(&stub)).await;
    (addr, stubs)
}

// ----- The proofs ------------------------------------------------------------

#[tokio::test]
async fn a_spoken_turn_is_heard_answered_and_spoken_back_a_piece_at_a_time() {
    let (addr, stubs) = setup(Stubs {
        transcript: "what is running".into(),
        chat: vec![final_reply(
            "Two sessions are running. Both are idle right now.",
        )],
        ..Stubs::default()
    })
    .await;
    let config: Value = client()
        .get(format!("http://{addr}/api/config"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(config["assistant_call_enabled"], json!(true));

    let mut ws = dial(&addr, TEST_TOKEN).await;
    let mut seen = Vec::new();
    let created = read_until(&mut ws, &mut seen, is("session.created")).await;
    assert_eq!(created["sample_rate"], json!(16_000));

    speak(&mut ws, &utterance()).await;
    let done = read_until(&mut ws, &mut seen, is("response.done")).await;
    let kinds = types(&seen);
    for expected in [
        "input_audio_buffer.speech_started",
        "input_audio_buffer.speech_stopped",
        "conversation.item.input_audio_transcription.completed",
        "response.created",
        "response.text.delta",
        "response.audio.start",
        "<binary>",
    ] {
        assert!(
            kinds.iter().any(|k| k == expected),
            "{expected} missing: {kinds:?}"
        );
    }
    let transcript = seen
        .iter()
        .find(|e| e["type"] == "conversation.item.input_audio_transcription.completed")
        .unwrap();
    assert_eq!(transcript["text"], "what is running");
    // Two sentences, two pieces, each with its audio right behind it.
    let pieces: Vec<&Value> = seen
        .iter()
        .filter(|e| e["type"] == "response.audio.start")
        .collect();
    assert_eq!(pieces.len(), 2, "{kinds:?}");
    assert_eq!(pieces[0]["text"], "Two sessions are running.");
    assert_eq!(pieces[0]["content_type"], "audio/wav");
    let first = kinds
        .iter()
        .position(|k| k == "response.audio.start")
        .unwrap();
    assert_eq!(kinds[first + 1], "<binary>");

    assert_eq!(done["status"], "completed");
    assert_eq!(
        done["text"],
        "Two sessions are running. Both are idle right now."
    );
    let metrics = &done["metrics"];
    assert!(
        metrics["speech_end_to_first_audio_ms"].as_u64().is_some(),
        "{done}"
    );
    assert!(metrics["endpoint_ms"].as_u64().is_some(), "{done}");

    // The call turn is an ordinary assistant turn in the conversation.
    let history: Value = client()
        .get(format!("http://{addr}/api/assistant/history"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let transcript = history["transcript"].as_array().unwrap();
    assert_eq!(transcript.len(), 2);
    assert_eq!(transcript[0]["text"], "what is running");
    // The model was asked as a call, for spoken sentences.
    assert_eq!(stubs.lock().unwrap().chat_calls, 1);
}

#[tokio::test]
async fn speaking_over_the_reply_stops_it_and_keeps_only_what_was_heard() {
    let (addr, stubs) = setup(Stubs {
        transcript: "tell me everything".into(),
        chat: vec![
            final_reply("First sentence here. Second sentence here. Third sentence here."),
            final_reply("Sure."),
        ],
        tts_delay_ms: 300,
        ..Stubs::default()
    })
    .await;
    let mut ws = dial(&addr, TEST_TOKEN).await;
    let mut seen = Vec::new();
    read_until(&mut ws, &mut seen, is("session.created")).await;

    speak(&mut ws, &utterance()).await;
    let first = read_until(&mut ws, &mut seen, is("response.audio.start")).await;
    let response_id = first["response_id"].as_str().unwrap().to_string();
    send(
        &mut ws,
        json!({"type": "output_audio.started", "response_id": response_id, "index": 0}),
    )
    .await;

    // The user talks over it, long enough to be meant.
    stubs.lock().unwrap().transcript = "wait, stop".into();
    let mut over = quiet(100);
    over.extend(tone(800, 0.5));
    speak(&mut ws, &over).await;
    let clear = read_until(&mut ws, &mut seen, is("output_audio.clear")).await;
    assert_eq!(clear["response_id"], json!(response_id));

    // The cut reply reports what was kept; the history agrees.
    let done = read_until(&mut ws, &mut seen, |e| {
        e["type"] == "response.done" && e["response_id"] == json!(response_id)
    })
    .await;
    assert_eq!(done["status"], "interrupted");
    assert_eq!(done["text"], "First sentence here.");
    let history: Value = client()
        .get(format!("http://{addr}/api/assistant/history"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let reply = &history["transcript"][1];
    assert_eq!(reply["text"], "First sentence here.");
    assert_eq!(reply["interrupted"], json!(true));

    // And the interruption becomes the next turn.
    speak(&mut ws, &quiet(1_000)).await;
    let next = read_until(&mut ws, &mut seen, |e| {
        e["type"] == "response.done" && e["response_id"] != json!(response_id)
    })
    .await;
    assert_eq!(next["text"], "Sure.");
}

#[tokio::test]
async fn a_spoken_yes_never_approves_and_the_button_does() {
    let (addr, stubs) = setup(Stubs::default()).await;
    // A terminal for the assistant to propose typing into.
    let session: Value = client()
        .post(format!("http://{addr}/api/sessions"))
        .json(&json!({"name": "shell", "command": ["/bin/cat"]}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let session_id = session["id"].as_str().unwrap().to_string();
    {
        let mut stubs = stubs.lock().unwrap();
        stubs.transcript = "list the files in my shell".into();
        stubs.chat = vec![
            json!({"role": "assistant", "content": null, "tool_calls": [{
                "id": "call_1",
                "type": "function",
                "function": {
                    "name": "send_input",
                    "arguments": json!({"session_id": session_id, "text": "ls"}).to_string(),
                },
            }]}),
            final_reply("Done, I typed it."),
        ];
    }
    let mut ws = dial(&addr, TEST_TOKEN).await;
    let mut seen = Vec::new();
    read_until(&mut ws, &mut seen, is("session.created")).await;

    speak(&mut ws, &utterance()).await;
    let card = read_until(&mut ws, &mut seen, is("assistant.pending_action")).await;
    let action_id = card["action"]["id"].as_str().unwrap().to_string();
    assert_eq!(card["action"]["kind"], "send_input");
    let done = read_until(&mut ws, &mut seen, is("response.done")).await;
    assert_eq!(done["status"], "pending_approval");
    let calls_before = stubs.lock().unwrap().chat_calls;

    // "Yes" is said, not pressed.
    stubs.lock().unwrap().transcript = "yes, do it".into();
    seen.clear();
    speak(&mut ws, &utterance()).await;
    let done = read_until(&mut ws, &mut seen, is("response.done")).await;
    assert_eq!(done["status"], "completed");
    assert!(
        done["text"].as_str().unwrap().contains("on your screen"),
        "{done}"
    );
    assert_eq!(
        stubs.lock().unwrap().chat_calls,
        calls_before,
        "a spoken turn while a card waits must not reach the model"
    );
    let history: Value = client()
        .get(format!("http://{addr}/api/assistant/history"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        history["pending_action"]["id"],
        json!(action_id),
        "the card is still waiting, neither approved nor abandoned"
    );

    // The button is what approves.
    seen.clear();
    send(
        &mut ws,
        json!({"type": "action.resolve", "id": action_id, "approve": true}),
    )
    .await;
    let resolved = read_until(&mut ws, &mut seen, is("assistant.action_resolved")).await;
    assert_eq!(resolved["approved"], json!(true));
    let done = read_until(&mut ws, &mut seen, is("response.done")).await;
    assert_eq!(done["text"], "Done, I typed it.");
    let history: Value = client()
        .get(format!("http://{addr}/api/assistant/history"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(history.get("pending_action").is_none());
    // A live PTY outlives the test's runtime unless it is stopped.
    client()
        .post(format!("http://{addr}/api/sessions/{session_id}/kill"))
        .send()
        .await
        .unwrap();
}

#[tokio::test]
async fn a_second_call_is_refused_while_one_is_live() {
    let (addr, _) = setup(Stubs::default()).await;
    let mut first = dial(&addr, TEST_TOKEN).await;
    let mut seen = Vec::new();
    read_until(&mut first, &mut seen, is("session.created")).await;
    let mut second = dial(&addr, TEST_TOKEN).await;
    let mut seen = Vec::new();
    let error = read_until(&mut second, &mut seen, is("error")).await;
    assert!(error["message"].as_str().unwrap().contains("already"));
    let close = read_until(&mut second, &mut seen, is("<close>")).await;
    assert!(close["frame"].as_str().unwrap().contains("4409"), "{close}");
}

#[tokio::test]
async fn a_wrong_token_is_closed_without_a_call() {
    let (addr, _) = setup(Stubs::default()).await;
    let mut ws = dial(&addr, "not-the-token-at-all-000").await;
    let mut seen = Vec::new();
    let close = read_until(&mut ws, &mut seen, is("<close>")).await;
    assert!(close["frame"].as_str().unwrap().contains("4401"), "{close}");
    assert!(!types(&seen).contains(&"session.created".to_string()));
}

#[tokio::test]
async fn no_speech_backend_means_no_call() {
    let stubs = Arc::new(Mutex::new(Stubs::default()));
    let stub = stub_server(stubs).await;
    let mut cfg = test_config(&stub);
    cfg.assistant_tts_base_urls = vec![];
    let addr = boot(cfg).await;
    let config: Value = client()
        .get(format!("http://{addr}/api/config"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(config["assistant_call_enabled"], json!(false));
    let status = client()
        .get(format!("http://{addr}/api/assistant/call"))
        .send()
        .await
        .unwrap()
        .status();
    assert_eq!(status, reqwest::StatusCode::NOT_FOUND);
}
