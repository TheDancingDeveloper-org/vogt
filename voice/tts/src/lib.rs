//! The provider seam for OpenAI-compatible text-to-speech backends.

use std::{
    io::Cursor,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

use async_trait::async_trait;
use bytes::Bytes;
use ort::session::Session;
use piper_rs::{ModelConfig, Piper};
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

/// A real in-process Piper/ONNX synthesizer.
///
/// The JSON model configuration and its neighboring ONNX file are loaded once
/// at startup. Synthesis executes through piper-rs in this process and emits a
/// valid 16-bit PCM WAV response. WAV is intentionally the only native output
/// format until an explicit, bounded encoder is added; requests for another
/// format are rejected rather than mislabeled.
///
/// Only single-file VITS voices (`<voice>.onnx` next to `<voice>.onnx.json`)
/// are supported. piper-rs 0.2 dropped the streaming encoder/decoder model
/// layout, so a config that declares `"streaming": true` is rejected at load.
pub struct PiperSynthesizer {
    // piper-rs 0.2 synthesizes through `&mut self`, so one inference runs at a
    // time per loaded voice.
    piper: Arc<Mutex<Piper>>,
    // The voice's own `inference.length_scale`, which `speed` divides.
    base_length_scale: f32,
    model_id: String,
}

impl std::fmt::Debug for PiperSynthesizer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PiperSynthesizer")
            .field("model_id", &self.model_id)
            .finish_non_exhaustive()
    }
}

impl PiperSynthesizer {
    pub fn new(
        config_path: impl Into<PathBuf>,
        model_id: impl Into<String>,
    ) -> Result<Self, SpeechError> {
        let config_path = config_path.into();
        if !config_path.is_file() {
            return Err(SpeechError::Provider(format!(
                "Piper model config does not exist: {}",
                config_path.display()
            )));
        }
        let config = load_model_config(&config_path)?;
        let model_path = onnx_path(&config_path)?;
        if !model_path.is_file() {
            return Err(SpeechError::Provider(format!(
                "Piper ONNX model does not exist: {}",
                model_path.display()
            )));
        }
        let session = Session::builder()
            .and_then(|mut builder| builder.commit_from_file(&model_path))
            .map_err(|error| SpeechError::Provider(format!("load Piper model: {error}")))?;
        let base_length_scale = config.inference.length_scale;
        Ok(Self {
            piper: Arc::new(Mutex::new(Piper::from_session(session, config))),
            base_length_scale,
            model_id: model_id.into(),
        })
    }
}

/// Parse a Piper voice config, rejecting the streaming (encoder/decoder)
/// layout that piper-rs 0.2 no longer runs.
fn load_model_config(config_path: &Path) -> Result<ModelConfig, SpeechError> {
    let raw = std::fs::read(config_path)
        .map_err(|error| SpeechError::Provider(format!("read Piper model config: {error}")))?;
    let value: serde_json::Value = serde_json::from_slice(&raw)
        .map_err(|error| SpeechError::Provider(format!("parse Piper model config: {error}")))?;
    if value.get("streaming").and_then(serde_json::Value::as_bool) == Some(true) {
        return Err(SpeechError::Provider(
            "streaming Piper models (encoder.onnx/decoder.onnx) are not supported".into(),
        ));
    }
    serde_json::from_value(value)
        .map_err(|error| SpeechError::Provider(format!("parse Piper model config: {error}")))
}

/// The ONNX weights live beside the config: `voice.onnx.json` -> `voice.onnx`
/// (the rule piper-rs 0.1's `from_config_path` applied).
fn onnx_path(config_path: &Path) -> Result<PathBuf, SpeechError> {
    let stem = config_path.file_stem().ok_or_else(|| {
        SpeechError::Provider(format!(
            "invalid Piper model config filename: {}",
            config_path.display()
        ))
    })?;
    Ok(config_path.with_file_name(stem))
}

#[async_trait]
impl SpeechBackend for PiperSynthesizer {
    async fn synthesize(&self, request: SpeechRequest) -> Result<SpeechResponse, SpeechError> {
        if request.model != self.model_id {
            return Err(SpeechError::UnsupportedModel(request.model));
        }
        if !request.response_format.eq_ignore_ascii_case("wav") {
            return Err(SpeechError::UnsupportedModel(format!(
                "response format {}; in-process Piper supports wav",
                request.response_format
            )));
        }
        let length_scale = length_scale(self.base_length_scale, request.speed)?;
        let piper = Arc::clone(&self.piper);
        tokio::task::spawn_blocking(move || {
            let (samples, sample_rate) = {
                let mut piper = piper
                    .lock()
                    .map_err(|_| SpeechError::Provider("Piper worker poisoned".into()))?;
                piper
                    .create(&request.input, false, None, length_scale, None, None)
                    .map_err(|error| SpeechError::Provider(format!("Piper inference: {error}")))?
            };
            if samples.is_empty() {
                return Err(SpeechError::Provider("Piper returned no audio".into()));
            }
            Ok(SpeechResponse {
                audio: Bytes::from(encode_wav(&samples, sample_rate)?),
                content_type: "audio/wav".into(),
            })
        })
        .await
        .map_err(|error| SpeechError::Provider(format!("Piper worker failed: {error}")))?
    }

    fn name(&self) -> &'static str {
        "piper-rs"
    }
}

/// Map the OpenAI-compatible `speed` (0.25..=4.0, 1.0 = normal) onto Piper's
/// `length_scale`, the model's own phoneme-duration multiplier: twice as fast
/// is half as long. `None` keeps the voice's configured default.
fn length_scale(base: f32, speed: Option<f32>) -> Result<Option<f32>, SpeechError> {
    let Some(speed) = speed else {
        return Ok(None);
    };
    if !speed.is_finite() || !(0.25..=4.0).contains(&speed) {
        return Err(SpeechError::UnsupportedModel(format!(
            "speed {speed} is outside the supported range 0.25..=4.0"
        )));
    }
    Ok(Some(base / speed))
}

/// Encode mono f32 PCM (nominally -1.0..=1.0) as a 16-bit PCM WAV file.
fn encode_wav(samples: &[f32], sample_rate: u32) -> Result<Vec<u8>, SpeechError> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let encode_error = |error: hound::Error| SpeechError::Provider(format!("encode WAV: {error}"));
    let mut buffer = Cursor::new(Vec::with_capacity(44 + samples.len() * 2));
    let mut writer = hound::WavWriter::new(&mut buffer, spec).map_err(encode_error)?;
    for &sample in samples {
        let sample = if sample.is_finite() { sample } else { 0.0 };
        writer
            .write_sample((sample.clamp(-1.0, 1.0) * f32::from(i16::MAX)) as i16)
            .map_err(encode_error)?;
    }
    writer.finalize().map_err(encode_error)?;
    Ok(buffer.into_inner())
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

    #[test]
    fn native_speed_is_validated_and_mapped_to_piper_length_scale() {
        assert!(length_scale(1.0, Some(0.24)).is_err());
        assert!(length_scale(1.0, Some(4.01)).is_err());
        assert!(length_scale(1.0, Some(f32::NAN)).is_err());
        assert_eq!(length_scale(1.0, None).unwrap(), None);
        assert_eq!(length_scale(1.0, Some(1.0)).unwrap(), Some(1.0));
        assert_eq!(length_scale(1.0, Some(0.25)).unwrap(), Some(4.0));
        assert_eq!(length_scale(1.0, Some(4.0)).unwrap(), Some(0.25));
        // A voice with a slower default keeps its proportions.
        assert_eq!(length_scale(1.2, Some(2.0)).unwrap(), Some(0.6));
    }

    #[test]
    fn wav_encoding_is_mono_16_bit_pcm_at_the_model_rate() {
        let wav = encode_wav(&[0.0, 1.0, -1.0, 2.0, f32::NAN], 22_050).unwrap();
        let mut reader = hound::WavReader::new(Cursor::new(wav)).unwrap();
        let spec = reader.spec();
        assert_eq!(spec.channels, 1);
        assert_eq!(spec.sample_rate, 22_050);
        assert_eq!(spec.bits_per_sample, 16);
        let samples: Vec<i16> = reader.samples::<i16>().map(Result::unwrap).collect();
        assert_eq!(samples, [0, i16::MAX, -i16::MAX, i16::MAX, 0]);
    }

    #[test]
    fn onnx_weights_sit_beside_the_config() {
        assert_eq!(
            onnx_path(Path::new("/m/en_US-ljspeech-medium.onnx.json")).unwrap(),
            Path::new("/m/en_US-ljspeech-medium.onnx")
        );
    }

    #[test]
    fn native_constructor_rejects_streaming_models_and_missing_weights() {
        let directory = tempfile::tempdir().unwrap();
        let streaming = directory.path().join("streaming.onnx.json");
        std::fs::write(&streaming, br#"{"streaming": true}"#).unwrap();
        let error = PiperSynthesizer::new(&streaming, "tts-1").unwrap_err();
        assert!(error.to_string().contains("streaming"), "{error}");

        let config = directory.path().join("voice.onnx.json");
        std::fs::write(
            &config,
            br#"{"audio": {"sample_rate": 22050}, "espeak": {"voice": "en-us"},
                "inference": {"noise_scale": 0.667, "length_scale": 1.0, "noise_w": 0.8},
                "num_speakers": 1, "speaker_id_map": {}, "phoneme_id_map": {}}"#,
        )
        .unwrap();
        let error = PiperSynthesizer::new(&config, "tts-1").unwrap_err();
        assert!(
            error.to_string().contains("ONNX model does not exist"),
            "{error}"
        );
    }

    #[test]
    fn native_constructor_reports_missing_model_config() {
        let directory = tempfile::tempdir().unwrap();
        let error =
            PiperSynthesizer::new(directory.path().join("missing.json"), "tts-1").unwrap_err();
        assert!(error.to_string().contains("does not exist"));
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

    /// A real model round trip. Unit tests carry no weights, so this runs only
    /// on request: point `VOGT_VOICE_TEST_PIPER_CONFIG` at a voice's
    /// `.onnx.json` (and `PIPER_ESPEAKNG_DATA_DIRECTORY` at the directory
    /// holding `espeak-ng-data`), then `cargo test -- --ignored`.
    #[tokio::test]
    #[ignore = "needs a Piper voice; set VOGT_VOICE_TEST_PIPER_CONFIG"]
    async fn piper_round_trip_produces_speech_and_honours_speed() {
        let config = std::env::var("VOGT_VOICE_TEST_PIPER_CONFIG")
            .expect("VOGT_VOICE_TEST_PIPER_CONFIG names a Piper .onnx.json");
        let backend = PiperSynthesizer::new(config, "tts-1").unwrap();
        let request = |speed| SpeechRequest {
            model: "tts-1".into(),
            voice: "alloy".into(),
            input: "Hello from the Vogt voice sidecar.".into(),
            response_format: "wav".into(),
            speed,
        };
        let duration = |audio: &Bytes| {
            let reader = hound::WavReader::new(Cursor::new(audio.to_vec())).unwrap();
            let spec = reader.spec();
            assert_eq!((spec.channels, spec.bits_per_sample), (1, 16));
            reader.duration() as f32 / spec.sample_rate as f32
        };
        let normal = backend.synthesize(request(None)).await.unwrap();
        assert_eq!(normal.content_type, "audio/wav");
        let normal = duration(&normal.audio);
        assert!(normal > 0.5, "{normal}s of speech");
        let fast = duration(&backend.synthesize(request(Some(2.0))).await.unwrap().audio);
        assert!(fast < normal * 0.75, "2x speed: {fast}s vs {normal}s");
    }
}
