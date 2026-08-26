use std::sync::Arc;

use axum::{
    extract::{Multipart, State},
    http::{header, HeaderValue},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use bytes::Bytes;
use serde::{Deserialize, Serialize};
use serde_json::json;
use vogt_voice_models::{ConfiguredModelCache, ModelCache, ModelInfo};
use vogt_voice_stt::{
    TranscriptionBackend, TranscriptionError, TranscriptionRequest, UnconfiguredTranscriber,
};
use vogt_voice_tts::{SpeechBackend, SpeechError, SpeechRequest, UnconfiguredSynthesizer};

use crate::{error::ApiError, Config};

pub struct AppState {
    pub config: Config,
    pub models: Arc<dyn ModelCache>,
    pub stt: Arc<dyn TranscriptionBackend>,
    pub tts: Arc<dyn SpeechBackend>,
}

impl AppState {
    pub fn from_config(config: Config) -> Self {
        let models = vec![
            ModelInfo::new(config.stt_model.clone(), "vogt-voice-stt"),
            ModelInfo::new(config.tts_model.clone(), "vogt-voice-tts"),
        ];
        Self {
            models: Arc::new(ConfiguredModelCache::new(
                config.model_cache_dir.clone(),
                deduplicate_models(models),
            )),
            config,
            stt: Arc::new(UnconfiguredTranscriber),
            tts: Arc::new(UnconfiguredSynthesizer),
        }
    }

    #[cfg(test)]
    fn with_backends(
        config: Config,
        stt: Arc<dyn TranscriptionBackend>,
        tts: Arc<dyn SpeechBackend>,
    ) -> Self {
        let mut state = Self::from_config(config);
        state.stt = stt;
        state.tts = tts;
        state
    }
}

fn deduplicate_models(models: Vec<ModelInfo>) -> Vec<ModelInfo> {
    let mut unique = Vec::with_capacity(models.len());
    for model in models {
        if !unique
            .iter()
            .any(|existing: &ModelInfo| existing.id == model.id)
        {
            unique.push(model);
        }
    }
    unique
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/v1/models", get(models))
        .route("/v1/audio/transcriptions", post(transcriptions))
        .route("/v1/audio/speech", post(speech))
        .with_state(Arc::new(state))
}

async fn health(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    Json(json!({
        "status": "ok",
        "service": "vogt-voice",
        "stt": { "provider": state.stt.name(), "ready": state.stt.name() != "unconfigured" },
        "tts": { "provider": state.tts.name(), "ready": state.tts.name() != "unconfigured" },
        "model_cache": state.models.root(),
    }))
}

#[derive(Debug, Serialize)]
struct ModelList {
    object: &'static str,
    data: Vec<ModelInfo>,
}

async fn models(State(state): State<Arc<AppState>>) -> Json<ModelList> {
    Json(ModelList {
        object: "list",
        data: state.models.list(),
    })
}

async fn transcriptions(
    State(state): State<Arc<AppState>>,
    mut multipart: Multipart,
) -> Result<Json<serde_json::Value>, ApiError> {
    let mut audio: Option<(Bytes, String, String)> = None;
    let mut model = None;
    let mut language = None;
    let mut prompt = None;

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|error| ApiError::bad_request(format!("invalid multipart body: {error}")))?
    {
        let name = field.name().unwrap_or_default().to_string();
        match name.as_str() {
            "file" => {
                let filename = field.file_name().unwrap_or("audio.webm").to_string();
                let content_type = field
                    .content_type()
                    .unwrap_or("application/octet-stream")
                    .to_string();
                let bytes = field.bytes().await.map_err(|error| {
                    ApiError::bad_request(format!("could not read audio field: {error}"))
                })?;
                audio = Some((bytes, filename, content_type));
            }
            "model" => model = Some(read_text_field(field, "model").await?),
            "language" => language = Some(read_text_field(field, "language").await?),
            "prompt" => prompt = Some(read_text_field(field, "prompt").await?),
            _ => {}
        }
    }

    let (audio, filename, content_type) =
        audio.ok_or_else(|| ApiError::bad_request("missing file field"))?;
    if audio.is_empty() {
        return Err(ApiError::bad_request("audio file is empty"));
    }
    let model = model.ok_or_else(|| ApiError::bad_request("missing model field"))?;
    let response = state
        .stt
        .transcribe(TranscriptionRequest {
            audio,
            filename,
            content_type,
            model,
            language,
            prompt,
        })
        .await
        .map_err(transcription_error)?;
    Ok(Json(json!({ "text": response.text })))
}

async fn read_text_field(
    field: axum::extract::multipart::Field<'_>,
    name: &str,
) -> Result<String, ApiError> {
    field
        .text()
        .await
        .map_err(|error| ApiError::bad_request(format!("could not read {name} field: {error}")))
}

fn transcription_error(error: TranscriptionError) -> ApiError {
    match error {
        TranscriptionError::Unconfigured => ApiError::not_implemented(
            "speech-to-text is not configured; install a vogt-voice STT backend",
        ),
        TranscriptionError::UnsupportedModel(model) => {
            ApiError::bad_request(format!("speech-to-text model is not supported: {model}"))
        }
        TranscriptionError::Provider(message) => ApiError::bad_gateway(message),
    }
}

#[derive(Debug, Deserialize)]
struct SpeechRequestBody {
    model: String,
    voice: String,
    input: String,
    #[serde(default = "default_response_format")]
    response_format: String,
    speed: Option<f32>,
}

fn default_response_format() -> String {
    "mp3".to_string()
}

async fn speech(
    State(state): State<Arc<AppState>>,
    Json(body): Json<SpeechRequestBody>,
) -> Result<Response, ApiError> {
    if body.input.trim().is_empty() {
        return Err(ApiError::bad_request("input must not be empty"));
    }
    if body.voice.trim().is_empty() {
        return Err(ApiError::bad_request("voice must not be empty"));
    }
    let response = state
        .tts
        .synthesize(SpeechRequest {
            model: body.model,
            voice: body.voice,
            input: body.input,
            response_format: body.response_format,
            speed: body.speed,
        })
        .await
        .map_err(speech_error)?;

    let content_type = HeaderValue::from_str(&response.content_type)
        .map_err(|_| ApiError::bad_gateway("provider returned an invalid content type"))?;
    Ok(([(header::CONTENT_TYPE, content_type)], response.audio).into_response())
}

fn speech_error(error: SpeechError) -> ApiError {
    match error {
        SpeechError::Unconfigured => ApiError::not_implemented(
            "text-to-speech is not configured; install a vogt-voice TTS backend",
        ),
        SpeechError::UnsupportedModel(model) => {
            ApiError::bad_request(format!("text-to-speech model is not supported: {model}"))
        }
        SpeechError::Provider(message) => ApiError::bad_gateway(message),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use bytes::Bytes;
    use clap::Parser;
    use tower::ServiceExt;
    use vogt_voice_stt::{TranscriptionResponse, UnconfiguredTranscriber};
    use vogt_voice_tts::UnconfiguredSynthesizer;

    use super::*;

    fn test_router() -> Router {
        router(AppState::with_backends(
            Config::parse_from(["test"]),
            Arc::new(UnconfiguredTranscriber),
            Arc::new(UnconfiguredSynthesizer),
        ))
    }

    #[tokio::test]
    async fn health_and_models_are_openai_sidecar_contracts() {
        let response = test_router()
            .clone()
            .oneshot(Request::get("/health").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let response = test_router()
            .oneshot(Request::get("/v1/models").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["object"], "list");
        assert!(json["data"]
            .as_array()
            .unwrap()
            .iter()
            .any(|model| model["id"] == "whisper-1"));
        assert!(json["data"]
            .as_array()
            .unwrap()
            .iter()
            .any(|model| model["id"] == "tts-1"));
    }

    #[tokio::test]
    async fn transcription_validates_multipart_and_returns_explicit_501() {
        let boundary = "voice-test-boundary";
        let body = format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"model\"\r\n\r\nwhisper-1\r\n--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"sample.wav\"\r\nContent-Type: audio/wav\r\n\r\nRIFF\r\n--{boundary}--\r\n"
        );
        let response = test_router()
            .oneshot(
                Request::post("/v1/audio/transcriptions")
                    .header(
                        "content-type",
                        format!("multipart/form-data; boundary={boundary}"),
                    )
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_IMPLEMENTED);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["error"]["code"], "provider_unconfigured");
        assert!(json["error"]["message"]
            .as_str()
            .unwrap()
            .contains("not configured"));
    }

    #[tokio::test]
    async fn speech_preserves_json_contract_and_returns_explicit_501() {
        let response = test_router()
            .oneshot(
                Request::post("/v1/audio/speech")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"model":"tts-1","voice":"alloy","input":"hello"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_IMPLEMENTED);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["error"]["code"], "provider_unconfigured");
    }

    #[tokio::test]
    async fn malformed_requests_are_400() {
        let response = test_router()
            .oneshot(
                Request::post("/v1/audio/speech")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"model":"tts-1","voice":"alloy","input":""}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn provider_contracts_are_object_safe() {
        let _stt: Arc<dyn TranscriptionBackend> = Arc::new(UnconfiguredTranscriber);
        let _tts: Arc<dyn SpeechBackend> = Arc::new(UnconfiguredSynthesizer);
        let _ = Bytes::new();
        let _ = TranscriptionResponse {
            text: String::new(),
        };
    }
}
