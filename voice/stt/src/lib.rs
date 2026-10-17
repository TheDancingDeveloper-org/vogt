//! The provider seam for OpenAI-compatible speech-to-text backends.
//!
//! Keeping this contract independent of the HTTP server means a future
//! whisper.cpp, Candle, or remote-provider implementation can be added
//! without changing the wire adapter.

use std::{path::Path, sync::Arc, time::Duration};

use async_trait::async_trait;
use bytes::Bytes;
use thiserror::Error;
use tokio::process::Command;

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

/// A real, opt-in STT adapter for a locally installed executable.
///
/// The command is an argv template, never a shell string. `{audio}` is
/// replaced with a temporary file containing the uploaded bytes; if omitted,
/// that path is appended. `{model}`, `{language}`, and `{prompt}` are replaced
/// with request metadata. The executable must write only the transcript to
/// stdout. This supports Whisper/Candle/Piper wrappers without coupling this
/// sidecar to a model runtime or pretending to perform inference itself.
#[derive(Debug, Clone)]
pub struct SubprocessTranscriber {
    command: Arc<Vec<String>>,
    timeout: Duration,
}

impl SubprocessTranscriber {
    pub fn new(command: Vec<String>, timeout: Duration) -> Result<Self, TranscriptionError> {
        validate_command(&command)?;
        Ok(Self {
            command: Arc::new(command),
            timeout,
        })
    }
}

#[async_trait]
impl TranscriptionBackend for SubprocessTranscriber {
    async fn transcribe(
        &self,
        request: TranscriptionRequest,
    ) -> Result<TranscriptionResponse, TranscriptionError> {
        let suffix = Path::new(&request.filename)
            .extension()
            .and_then(|extension| extension.to_str())
            .filter(|extension| {
                extension.len() <= 16 && extension.chars().all(|c| c.is_ascii_alphanumeric())
            })
            .map(|extension| format!(".{extension}"));
        let mut builder = tempfile::Builder::new();
        if let Some(suffix) = suffix.as_deref() {
            builder.suffix(suffix);
        }
        let audio_file = builder
            .tempfile()
            .map_err(|error| TranscriptionError::Provider(format!("create audio file: {error}")))?;
        tokio::fs::write(audio_file.path(), &request.audio)
            .await
            .map_err(|error| TranscriptionError::Provider(format!("write audio file: {error}")))?;

        let audio_path = audio_file.path().to_string_lossy().into_owned();
        let args = render_command(
            &self.command,
            &audio_path,
            &request.model,
            request.language.as_deref().unwrap_or(""),
            request.prompt.as_deref().unwrap_or(""),
        );
        let child = Command::new(&args[0])
            .args(&args[1..])
            .kill_on_drop(true)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|error| TranscriptionError::Provider(format!("start STT command: {error}")))?;
        let output = tokio::time::timeout(self.timeout, child.wait_with_output())
            .await
            .map_err(|_| TranscriptionError::Provider("STT command timed out".into()))?
            .map_err(|error| {
                TranscriptionError::Provider(format!("wait for STT command: {error}"))
            })?;
        if !output.status.success() {
            return Err(TranscriptionError::Provider(command_failure(
                &output.stderr,
            )));
        }
        let text = String::from_utf8(output.stdout).map_err(|_| {
            TranscriptionError::Provider("STT command returned non-UTF-8 stdout".into())
        })?;
        let text = text.trim().to_string();
        if text.is_empty() {
            return Err(TranscriptionError::Provider(
                "STT command returned an empty transcript".into(),
            ));
        }
        Ok(TranscriptionResponse { text })
    }

    fn name(&self) -> &'static str {
        "subprocess"
    }
}

fn validate_command(command: &[String]) -> Result<(), TranscriptionError> {
    if command.is_empty() || command[0].trim().is_empty() {
        return Err(TranscriptionError::Provider(
            "STT command must not be empty".into(),
        ));
    }
    if command.iter().any(|arg| arg.contains('\0')) {
        return Err(TranscriptionError::Provider(
            "STT command contains a NUL byte".into(),
        ));
    }
    Ok(())
}

fn render_command(
    command: &[String],
    audio: &str,
    model: &str,
    language: &str,
    prompt: &str,
) -> Vec<String> {
    let replacements = [
        ("{audio}", audio),
        ("{model}", model),
        ("{language}", language),
        ("{prompt}", prompt),
    ];
    let mut rendered = Vec::with_capacity(command.len() + 1);
    let mut has_audio = false;
    for arg in command {
        let mut value = arg.clone();
        for (placeholder, replacement) in replacements {
            if value.contains(placeholder) {
                value = value.replace(placeholder, replacement);
                has_audio |= placeholder == "{audio}";
            }
        }
        rendered.push(value);
    }
    if !has_audio {
        rendered.push(audio.to_string());
    }
    rendered
}

fn command_failure(stderr: &[u8]) -> String {
    let detail = String::from_utf8_lossy(stderr).trim().to_string();
    if detail.is_empty() {
        "STT command exited unsuccessfully".into()
    } else {
        format!(
            "STT command failed: {}",
            detail.chars().take(512).collect::<String>()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> TranscriptionRequest {
        TranscriptionRequest {
            audio: Bytes::from_static(b"real input bytes"),
            filename: "take.wav".into(),
            content_type: "audio/wav".into(),
            model: "whisper-1".into(),
            language: Some("en".into()),
            prompt: None,
        }
    }

    #[test]
    fn command_requires_an_executable() {
        assert!(SubprocessTranscriber::new(vec![], Duration::from_secs(1)).is_err());
        assert!(SubprocessTranscriber::new(vec!["".into()], Duration::from_secs(1)).is_err());
    }

    #[test]
    fn audio_path_is_appended_when_template_does_not_contain_it() {
        let rendered = render_command(
            &["whisper".into(), "--model".into(), "{model}".into()],
            "/tmp/input.wav",
            "base",
            "",
            "",
        );
        assert_eq!(rendered, ["whisper", "--model", "base", "/tmp/input.wav"]);
    }

    #[test]
    fn placeholders_are_replaced_without_shell_parsing() {
        let rendered = render_command(
            &["decoder".into(), "{audio}:{language}:{prompt}".into()],
            "/tmp/input.wav",
            "base",
            "en",
            "say hello; do not execute",
        );
        assert_eq!(
            rendered,
            ["decoder", "/tmp/input.wav:en:say hello; do not execute"]
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn subprocess_reads_the_real_audio_file_and_returns_stdout() {
        let backend = SubprocessTranscriber::new(
            vec![
                "/bin/sh".into(),
                "-c".into(),
                "test \"$(cat \"$1\")\" = 'real input bytes' && printf 'hello from whisper\\n'"
                    .into(),
                "vogt-test".into(),
                "{audio}".into(),
            ],
            Duration::from_secs(2),
        )
        .unwrap();
        let result = backend.transcribe(request()).await.unwrap();
        assert_eq!(result.text, "hello from whisper");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn subprocess_rejects_empty_stdout_and_timeout() {
        let empty = SubprocessTranscriber::new(
            vec!["/bin/sh".into(), "-c".into(), "exit 0".into()],
            Duration::from_secs(2),
        )
        .unwrap();
        assert!(empty.transcribe(request()).await.is_err());

        let slow = SubprocessTranscriber::new(
            vec!["/bin/sh".into(), "-c".into(), "sleep 2".into()],
            Duration::from_millis(10),
        )
        .unwrap();
        let error = slow.transcribe(request()).await.unwrap_err();
        assert!(error.to_string().contains("timed out"));
    }
}
