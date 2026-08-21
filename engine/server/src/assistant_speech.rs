//! Server-side speech (FR-T12): the engine as a thin proxy in front of an
//! OpenAI-compatible audio provider, so a client with no on-device STT/TTS can
//! still take a voice turn.
//!
//! ## Why this is not the chat profile
//!
//! The whole point of FR-T12 is that speech is configured *independently* of
//! the chat profile. A deployment can front chat through OpenRouter — which
//! does not serve `/audio/transcriptions` and `/audio/speech` uniformly — while
//! pointing STT and TTS at their own provider entirely: OpenAI direct, The Claw
//! Bay, or a local Whisper.cpp + Kokoro pair on loopback. The base URL, key and
//! model for each half live in their own config keys (`assistant_stt_*`,
//! `assistant_tts_*`) and are never read from `assistant_profiles`.
//!
//! ## Ordered fallback, voicemode semantics
//!
//! Each half carries an **ordered list** of base URLs, adopting voicemode's
//! `VOICEMODE_STT_BASE_URLS` / `VOICEMODE_TTS_BASE_URLS` behaviour: the engine
//! tries entry 1, and on a connection failure or non-2xx moves to the next —
//! local Whisper.cpp / Kokoro first, cloud (OpenAI) as the fallback. The key
//! (when set) is reused for whichever entry needs one, the cloud endpoint; a
//! local entry needs none, so an entry may succeed with no key.
//!
//! ## Unconfigured ⇒ 404
//!
//! A half is enabled only when its list is non-empty. An empty list, or every
//! entry failing, answers 404 — exactly as the assistant routes 404 when no key
//! is configured — so a client that finds the route missing falls back to
//! on-device speech or, failing that, typed input (FR-T6). The two halves are
//! independent: a deployment may configure STT and not TTS, or the reverse.
//!
//! ## Audio is not stored
//!
//! The bytes flow through this proxy and are never written anywhere. The
//! FR-T14 interaction log stays text-only (its own module says so, and this one
//! never touches it). There is deliberately no debug flag to retain audio: the
//! requirement's "unless a debug flag says so" is an allowance, not an
//! obligation, and the safe default — the only behaviour here — is to keep no
//! recording of anybody's voice.

use std::sync::Arc;
use std::time::Duration;

use axum::{
    extract::{Multipart, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::app::AppState;
use crate::config::Config;
use crate::error::{ApiError, Result};

/// One configured half: an ordered list of OpenAI-compatible base URLs to try,
/// the key reused for whichever needs one, and the model/voice to name in the
/// request.
struct AudioBackend {
    /// Ordered fallback list — entry 1 first, later entries on failure.
    base_urls: Vec<String>,
    /// Reused for whichever entry needs one (the cloud endpoint); `None` when
    /// only local, keyless entries are configured.
    api_key: Option<String>,
    model: String,
    /// Voice name — meaningful for TTS (`/audio/speech` requires one) and
    /// unused for STT, where it is left empty.
    voice: String,
}

impl AudioBackend {
    /// Configured iff it has at least one base URL to try.
    fn from_parts(
        base_urls: &[String],
        api_key: &Option<String>,
        model: &str,
        voice: &str,
    ) -> Option<Self> {
        if base_urls.is_empty() {
            return None;
        }
        Some(Self {
            base_urls: base_urls.to_vec(),
            api_key: api_key.clone(),
            model: model.to_string(),
            voice: voice.to_string(),
        })
    }
}

/// The server-side speech proxy. Holds one shared HTTP client and whichever
/// halves are configured. `None` for a half means that half 404s.
pub struct AssistantSpeech {
    client: reqwest::Client,
    stt: Option<AudioBackend>,
    tts: Option<AudioBackend>,
    /// Bounded per-attempt timeout, applied to each upstream entry so a hanging
    /// or dead endpoint cannot stall the request (config
    /// `assistant_speech_attempt_timeout_ms`).
    attempt_timeout: Duration,
}

impl AssistantSpeech {
    /// Build the proxy from config, or `None` when neither half is configured —
    /// in which case both routes 404 and the client falls back (FR-T6). A half
    /// is enabled when its base-URL list is non-empty; the config loader has
    /// already refused a key set against an empty list (r20).
    pub fn from_config(cfg: &Config) -> Option<Arc<Self>> {
        let stt = AudioBackend::from_parts(
            &cfg.assistant_stt_base_urls,
            &cfg.assistant_stt_api_key,
            &cfg.assistant_stt_model,
            "",
        );
        let tts = AudioBackend::from_parts(
            &cfg.assistant_tts_base_urls,
            &cfg.assistant_tts_api_key,
            &cfg.assistant_tts_model,
            &cfg.assistant_tts_voice,
        );
        if stt.is_none() && tts.is_none() {
            return None;
        }
        // No client-wide timeout: the bound that matters is *per attempt*
        // (below), applied to each upstream entry so one hanging endpoint does
        // not consume the budget the fallback entries need.
        let client = reqwest::Client::builder()
            .build()
            .expect("assistant speech http client");
        Some(Arc::new(Self {
            client,
            stt,
            tts,
            attempt_timeout: Duration::from_millis(cfg.assistant_speech_attempt_timeout_ms),
        }))
    }

    /// True when server-side transcription is configured, for `/api/config`.
    pub fn stt_enabled(&self) -> bool {
        self.stt.is_some()
    }

    /// True when server-side synthesis is configured, for `/api/config`.
    pub fn tts_enabled(&self) -> bool {
        self.tts.is_some()
    }
}

#[derive(Debug, Serialize)]
pub struct SttResponse {
    pub text: String,
}

/// `POST /api/assistant/stt` — multipart audio upload → `{ "text": ... }`.
///
/// Proxies to `/audio/transcriptions` on each configured base URL in order,
/// stopping at the first that answers 2xx (local first, cloud fallback). 404
/// when STT is unconfigured or every entry fails, so the client falls back
/// (FR-T6). The audio is forwarded and never stored.
pub async fn stt(
    State(state): State<Arc<AppState>>,
    mut multipart: Multipart,
) -> Result<Json<SttResponse>> {
    // Both the whole proxy being absent and this specific half being absent
    // read as 404: the route is simply not provisioned.
    let speech = state.assistant_speech.as_ref().ok_or(ApiError::NotFound)?;
    let backend = speech.stt.as_ref().ok_or(ApiError::NotFound)?;

    // Take the first file-bearing field. A voice client sends one audio blob;
    // we do not care what it named the field, only that it carried bytes.
    let mut audio: Option<(Vec<u8>, String, String)> = None;
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| ApiError::BadRequest(format!("reading upload: {e}")))?
    {
        let file_name = field
            .file_name()
            .map(str::to_owned)
            .unwrap_or_else(|| "audio.webm".to_string());
        let content_type = field
            .content_type()
            .map(str::to_owned)
            .unwrap_or_else(|| "application/octet-stream".to_string());
        let bytes = field
            .bytes()
            .await
            .map_err(|e| ApiError::BadRequest(format!("reading upload bytes: {e}")))?;
        if !bytes.is_empty() {
            audio = Some((bytes.to_vec(), file_name, content_type));
            break;
        }
    }
    let (bytes, file_name, content_type) =
        audio.ok_or_else(|| ApiError::BadRequest("no audio in upload".into()))?;

    // Try each base URL in order. A `multipart::Form` is consumed on send, so
    // it is rebuilt per attempt from the bytes we already hold.
    let mut last_error = String::new();
    for base_url in &backend.base_urls {
        let part = reqwest::multipart::Part::bytes(bytes.clone())
            .file_name(file_name.clone())
            .mime_str(&content_type)
            .unwrap_or_else(|_| {
                reqwest::multipart::Part::bytes(bytes.clone())
                    .file_name("audio.webm")
                    .mime_str("application/octet-stream")
                    .expect("octet-stream is a valid mime")
            });
        let form = reqwest::multipart::Form::new()
            .text("model", backend.model.clone())
            .part("file", part);

        let url = format!("{}/audio/transcriptions", base_url.trim_end_matches('/'));
        let mut request = speech
            .client
            .post(&url)
            .timeout(speech.attempt_timeout)
            .multipart(form);
        if let Some(key) = backend.api_key.as_deref() {
            request = request.bearer_auth(key);
        }
        match request.send().await {
            Ok(resp) if resp.status().is_success() => {
                let payload: Value = resp
                    .json()
                    .await
                    .map_err(|e| ApiError::BadGateway(format!("stt backend body: {e}")))?;
                let text = payload
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                return Ok(Json(SttResponse { text }));
            }
            Ok(resp) => {
                // Non-2xx: record and fall through to the next entry.
                let status = resp.status();
                let detail = resp.text().await.unwrap_or_default();
                last_error = format!("{url} → HTTP {status}: {}", truncate(&detail, 200));
            }
            Err(e) => last_error = format!("{url} → {e}"),
        }
    }
    // Every entry failed. Reported as 404 so the client falls back (FR-T6); the
    // reason is logged for an operator, not returned.
    tracing::warn!(last_error = %last_error, "all STT backends failed");
    Err(ApiError::NotFound)
}

#[derive(Debug, Deserialize)]
pub struct TtsReq {
    pub text: String,
}

/// `POST /api/assistant/tts` — `{ "text": ... }` → an audio stream.
///
/// Proxies to `/audio/speech` on each configured base URL in order, stopping at
/// the first that answers 2xx (local first, cloud fallback). 404 when TTS is
/// unconfigured or every entry fails, so the client falls back (FR-T6). The
/// synthesised audio is streamed back to the caller and never stored.
pub async fn tts(State(state): State<Arc<AppState>>, Json(req): Json<TtsReq>) -> Result<Response> {
    let speech = state.assistant_speech.as_ref().ok_or(ApiError::NotFound)?;
    let backend = speech.tts.as_ref().ok_or(ApiError::NotFound)?;

    if req.text.trim().is_empty() {
        return Err(ApiError::BadRequest("no text to speak".into()));
    }

    let body = json!({
        "model": backend.model,
        "input": req.text,
        "voice": backend.voice,
        "response_format": "mp3",
    });

    let mut last_error = String::new();
    for base_url in &backend.base_urls {
        let url = format!("{}/audio/speech", base_url.trim_end_matches('/'));
        let mut request = speech
            .client
            .post(&url)
            .timeout(speech.attempt_timeout)
            .json(&body);
        if let Some(key) = backend.api_key.as_deref() {
            request = request.bearer_auth(key);
        }
        match request.send().await {
            Ok(resp) if resp.status().is_success() => {
                let content_type = resp
                    .headers()
                    .get(header::CONTENT_TYPE)
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("audio/mpeg")
                    .to_string();
                let audio = resp
                    .bytes()
                    .await
                    .map_err(|e| ApiError::BadGateway(format!("tts backend body: {e}")))?;
                // `resp.bytes()` already yields a `bytes::Bytes`, which axum
                // turns into a body directly — no re-wrapping needed.
                return Ok((
                    StatusCode::OK,
                    [(header::CONTENT_TYPE, content_type)],
                    audio,
                )
                    .into_response());
            }
            Ok(resp) => {
                let status = resp.status();
                let detail = resp.text().await.unwrap_or_default();
                last_error = format!("{url} → HTTP {status}: {}", truncate(&detail, 200));
            }
            Err(e) => last_error = format!("{url} → {e}"),
        }
    }
    // Every entry failed. Reported as 404 so the client falls back (FR-T6).
    tracing::warn!(last_error = %last_error, "all TTS backends failed");
    Err(ApiError::NotFound)
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        // Respect char boundaries so a multibyte tail never panics the slice.
        let mut end = max;
        while end > 0 && !s.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &s[..end])
    }
}
