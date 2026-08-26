//! The provider seam for OpenAI-compatible speech-to-text backends.
//!
//! Keeping this contract independent of the HTTP server means a future
//! whisper.cpp, Candle, or remote-provider implementation can be added
//! without changing the wire adapter.

use async_trait::async_trait;
use bytes::Bytes;
use thiserror::Error;

#[derive(Debug, Clone)]
pub struct TranscriptionRequest {
    pub audio: Bytes,
    pub filename: String,
    pub content_type: String,
    pub model: String,
    pub language: Option<String>,
    pub prompt: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptionResponse {
    pub text: String,
}

#[derive(Debug, Error)]
pub enum TranscriptionError {
    #[error("speech-to-text provider is not configured")]
    Unconfigured,
    #[error("speech-to-text model is not supported: {0}")]
    UnsupportedModel(String),
    #[error("speech-to-text provider failed: {0}")]
    Provider(String),
}

/// A speech-to-text implementation. The server owns the HTTP contract; this
/// trait owns only inference inputs and outputs.
#[async_trait]
pub trait TranscriptionBackend: Send + Sync {
    async fn transcribe(
        &self,
        request: TranscriptionRequest,
    ) -> Result<TranscriptionResponse, TranscriptionError>;

    fn name(&self) -> &'static str;
}

/// Safe default until a real inference backend is deliberately installed.
#[derive(Debug, Default)]
pub struct UnconfiguredTranscriber;

#[async_trait]
impl TranscriptionBackend for UnconfiguredTranscriber {
    async fn transcribe(
        &self,
        _request: TranscriptionRequest,
    ) -> Result<TranscriptionResponse, TranscriptionError> {
        Err(TranscriptionError::Unconfigured)
    }

    fn name(&self) -> &'static str {
        "unconfigured"
    }
}
