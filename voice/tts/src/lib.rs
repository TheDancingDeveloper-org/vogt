//! The provider seam for OpenAI-compatible text-to-speech backends.

use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use bytes::Bytes;
use thiserror::Error;
use tokio::{io::AsyncWriteExt, process::Command};

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

/// A real, opt-in TTS adapter for a locally installed executable.
///
/// The command is an argv template, never a shell string. `{model}`,
/// `{voice}`, and `{format}` are replaced in its arguments. The request text
/// is written to stdin and the executable must write the requested audio
/// encoding to stdout. This makes Piper and site-specific wrappers usable
/// without coupling this sidecar to one model runtime.
#[derive(Debug, Clone)]
pub struct SubprocessSynthesizer {
    command: Arc<Vec<String>>,
    timeout: Duration,
}

impl SubprocessSynthesizer {
    pub fn new(command: Vec<String>, timeout: Duration) -> Result<Self, SpeechError> {
        validate_command(&command)?;
        Ok(Self {
            command: Arc::new(command),
            timeout,
        })
    }
}

#[async_trait]
impl SpeechBackend for SubprocessSynthesizer {
    async fn synthesize(&self, request: SpeechRequest) -> Result<SpeechResponse, SpeechError> {
        let args = render_command(&self.command, &request);
        let mut child = Command::new(&args[0])
            .args(&args[1..])
            .kill_on_drop(true)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|error| SpeechError::Provider(format!("start TTS command: {error}")))?;
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| SpeechError::Provider("TTS command has no stdin".into()))?;
        stdin
            .write_all(request.input.as_bytes())
            .await
            .map_err(|error| SpeechError::Provider(format!("write TTS input: {error}")))?;
        drop(stdin);
        let output = tokio::time::timeout(self.timeout, child.wait_with_output())
            .await
            .map_err(|_| SpeechError::Provider("TTS command timed out".into()))?
            .map_err(|error| SpeechError::Provider(format!("wait for TTS command: {error}")))?;
        if !output.status.success() {
            return Err(SpeechError::Provider(command_failure(&output.stderr)));
        }
        if output.stdout.is_empty() {
            return Err(SpeechError::Provider(
                "TTS command returned no audio".into(),
            ));
        }
        Ok(SpeechResponse {
            audio: Bytes::from(output.stdout),
            content_type: content_type(&request.response_format)?,
        })
    }

    fn name(&self) -> &'static str {
        "subprocess"
    }
}

fn validate_command(command: &[String]) -> Result<(), SpeechError> {
    if command.is_empty() || command[0].trim().is_empty() {
        return Err(SpeechError::Provider(
            "TTS command must not be empty".into(),
        ));
    }
    if command.iter().any(|arg| arg.contains('\0')) {
        return Err(SpeechError::Provider(
            "TTS command contains a NUL byte".into(),
        ));
    }
    Ok(())
}

fn render_command(command: &[String], request: &SpeechRequest) -> Vec<String> {
    command
        .iter()
        .map(|arg| {
            arg.replace("{model}", &request.model)
                .replace("{voice}", &request.voice)
                .replace("{format}", &request.response_format)
        })
        .collect()
}

fn content_type(format: &str) -> Result<String, SpeechError> {
    match format.to_ascii_lowercase().as_str() {
        "mp3" => Ok("audio/mpeg".into()),
        "wav" => Ok("audio/wav".into()),
        "opus" => Ok("audio/opus".into()),
        "ogg" => Ok("audio/ogg".into()),
        "flac" => Ok("audio/flac".into()),
        other => Err(SpeechError::UnsupportedModel(format!(
            "unsupported response format: {other}"
        ))),
    }
}

fn command_failure(stderr: &[u8]) -> String {
    let detail = String::from_utf8_lossy(stderr).trim().to_string();
    if detail.is_empty() {
        "TTS command exited unsuccessfully".into()
    } else {
        format!(
            "TTS command failed: {}",
            detail.chars().take(512).collect::<String>()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_requires_an_executable() {
        assert!(SubprocessSynthesizer::new(vec![], Duration::from_secs(1)).is_err());
        assert!(SubprocessSynthesizer::new(vec!["".into()], Duration::from_secs(1)).is_err());
    }

    #[test]
    fn command_templates_replace_metadata_but_not_text() {
        let request = SpeechRequest {
            model: "piper".into(),
            voice: "en_US-lessac".into(),
            input: "hello; do not parse me".into(),
            response_format: "wav".into(),
            speed: None,
        };
        assert_eq!(
            render_command(
                &[
                    "tts".into(),
                    "{model}".into(),
                    "{voice}".into(),
                    "{format}".into()
                ],
                &request
            ),
            ["tts", "piper", "en_US-lessac", "wav"]
        );
    }

    #[test]
    fn response_formats_have_real_audio_content_types() {
        assert_eq!(content_type("mp3").unwrap(), "audio/mpeg");
        assert_eq!(content_type("wav").unwrap(), "audio/wav");
        assert!(content_type("text").is_err());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn subprocess_writes_text_to_stdin_and_returns_real_audio_bytes() {
        let backend = SubprocessSynthesizer::new(
            vec![
                "/bin/sh".into(),
                "-c".into(),
                "read text; test \"$text\" = 'hello from vogt' && printf '\\001\\002\\003'".into(),
            ],
            Duration::from_secs(2),
        )
        .unwrap();
        let result = backend
            .synthesize(SpeechRequest {
                model: "piper".into(),
                voice: "en_US-lessac".into(),
                input: "hello from vogt".into(),
                response_format: "wav".into(),
                speed: None,
            })
            .await
            .unwrap();
        assert_eq!(result.audio, Bytes::from_static(b"\x01\x02\x03"));
        assert_eq!(result.content_type, "audio/wav");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn subprocess_rejects_empty_audio_and_timeout() {
        let empty = SubprocessSynthesizer::new(
            vec!["/bin/sh".into(), "-c".into(), "cat >/dev/null".into()],
            Duration::from_secs(2),
        )
        .unwrap();
        let request = SpeechRequest {
            model: "piper".into(),
            voice: "alloy".into(),
            input: "hello".into(),
            response_format: "mp3".into(),
            speed: None,
        };
        assert!(empty.synthesize(request.clone()).await.is_err());

        let slow = SubprocessSynthesizer::new(
            vec![
                "/bin/sh".into(),
                "-c".into(),
                "cat >/dev/null; sleep 2".into(),
            ],
            Duration::from_millis(10),
        )
        .unwrap();
        let error = slow.synthesize(request).await.unwrap_err();
        assert!(error.to_string().contains("timed out"));
    }
}
