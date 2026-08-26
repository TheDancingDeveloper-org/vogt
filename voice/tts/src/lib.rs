//! The provider seam for OpenAI-compatible text-to-speech backends.

use async_trait::async_trait;
use bytes::Bytes;
use thiserror::Error;

#[derive(Debug, Clone)]
pub struct SpeechRequest {
    pub model: String,
    pub voice: String,
    pub input: String,
    pub response_format: String,
    pub speed: Option<f32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpeechResponse {
    pub audio: Bytes,
    pub content_type: String,
}

#[derive(Debug, Error)]
pub enum SpeechError {
    #[error("text-to-speech provider is not configured")]
    Unconfigured,
    #[error("text-to-speech model is not supported: {0}")]
    UnsupportedModel(String),
    #[error("text-to-speech provider failed: {0}")]
    Provider(String),
}

/// A text-to-speech implementation. Audio encoding and model execution stay
/// behind this boundary; the HTTP server only translates the wire request.
#[async_trait]
pub trait SpeechBackend: Send + Sync {
    async fn synthesize(&self, request: SpeechRequest) -> Result<SpeechResponse, SpeechError>;

    fn name(&self) -> &'static str;
}

/// Safe default until a real Piper/Kokoro/ONNX backend is deliberately
/// installed.
#[derive(Debug, Default)]
pub struct UnconfiguredSynthesizer;

#[async_trait]
impl SpeechBackend for UnconfiguredSynthesizer {
    async fn synthesize(&self, _request: SpeechRequest) -> Result<SpeechResponse, SpeechError> {
        Err(SpeechError::Unconfigured)
    }

    fn name(&self) -> &'static str {
        "unconfigured"
    }
}
