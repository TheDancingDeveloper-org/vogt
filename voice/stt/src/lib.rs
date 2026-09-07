//! The provider seam for OpenAI-compatible speech-to-text backends.
//!
//! Keeping this contract independent of the HTTP server means a future
//! whisper.cpp, Candle, or remote-provider implementation can be added
//! without changing the wire adapter.

use std::{
    io::Cursor,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use async_trait::async_trait;
use bytes::Bytes;
use opus::{Channels as OpusChannels, Decoder as OpusDecoder};
use symphonia::core::{
    audio::{AudioBufferRef, SampleBuffer},
    codecs::DecoderOptions,
    codecs::CODEC_TYPE_OPUS,
    errors::Error as SymphoniaError,
    formats::FormatOptions,
    formats::FormatReader,
    io::{MediaSourceStream, MediaSourceStreamOptions},
    meta::MetadataOptions,
    probe::Hint,
};
use symphonia::default::{get_codecs, get_probe};
use thiserror::Error;
use tokio::process::Command;
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

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

/// A real in-process Whisper.cpp transcriber.
///
/// The model is loaded once when the sidecar starts. Requests accept common
/// browser audio containers (WAV, WebM/Opus, Ogg/Opus, and Ogg/Vorbis), decode
/// them in this process, convert them to mono 16 kHz samples, and run Whisper
/// directly in the sidecar; no executable or shell is involved.
pub struct WhisperTranscriber {
    context: Arc<WhisperContext>,
    model_id: String,
    threads: usize,
}

impl std::fmt::Debug for WhisperTranscriber {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WhisperTranscriber")
            .field("model_id", &self.model_id)
            .field("threads", &self.threads)
            .finish_non_exhaustive()
    }
}

impl WhisperTranscriber {
    pub fn new(
        model_path: impl Into<PathBuf>,
        model_id: impl Into<String>,
        threads: usize,
    ) -> Result<Self, TranscriptionError> {
        let model_path = model_path.into();
        if !model_path.is_file() {
            return Err(TranscriptionError::Provider(format!(
                "Whisper model does not exist: {}",
                model_path.display()
            )));
        }
        if threads == 0 {
            return Err(TranscriptionError::Provider(
                "Whisper thread count must be at least one".into(),
            ));
        }
        let path = model_path.to_str().ok_or_else(|| {
            TranscriptionError::Provider("Whisper model path is not valid UTF-8".into())
        })?;
        let context = WhisperContext::new_with_params(path, WhisperContextParameters::default())
            .map_err(|error| {
                TranscriptionError::Provider(format!("load Whisper model: {error}"))
            })?;
        Ok(Self {
            context: Arc::new(context),
            model_id: model_id.into(),
            threads,
        })
    }
}

#[async_trait]
impl TranscriptionBackend for WhisperTranscriber {
    async fn transcribe(
        &self,
        request: TranscriptionRequest,
    ) -> Result<TranscriptionResponse, TranscriptionError> {
        if request.model != self.model_id {
            return Err(TranscriptionError::UnsupportedModel(request.model));
        }
        let samples = decode_audio_mono_16khz(&request.audio, &request.filename)?;
        let context = Arc::clone(&self.context);
        let language = request.language;
        let prompt = request.prompt;
        let threads = self.threads;
        tokio::task::spawn_blocking(move || {
            let mut state = context.create_state().map_err(|error| {
                TranscriptionError::Provider(format!("create Whisper state: {error}"))
            })?;
            let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
            params.set_n_threads(threads as i32);
            params.set_no_context(true);
            params.set_no_timestamps(true);
            params.set_print_progress(false);
            params.set_print_realtime(false);
            if let Some(language) = language.as_deref() {
                params.set_language(Some(language));
            }
            if let Some(prompt) = prompt.as_deref() {
                params.set_initial_prompt(prompt);
            }
            state.full(params, &samples).map_err(|error| {
                TranscriptionError::Provider(format!("Whisper inference: {error}"))
            })?;
            let count = state.full_n_segments().map_err(|error| {
                TranscriptionError::Provider(format!("read Whisper segments: {error}"))
            })?;
            let mut text = String::new();
            for index in 0..count {
                let segment = state.full_get_segment_text(index).map_err(|error| {
                    TranscriptionError::Provider(format!("read Whisper transcript: {error}"))
                })?;
                text.push_str(&segment);
            }
            let text = text.trim().to_string();
            if text.is_empty() {
                return Err(TranscriptionError::Provider(
                    "Whisper returned an empty transcript".into(),
                ));
            }
            Ok(TranscriptionResponse { text })
        })
        .await
        .map_err(|error| TranscriptionError::Provider(format!("Whisper worker failed: {error}")))?
    }

    fn name(&self) -> &'static str {
        "whisper-rs"
    }
}

fn decode_audio_mono_16khz(audio: &[u8], filename: &str) -> Result<Vec<f32>, TranscriptionError> {
    if is_wav(audio, filename) {
        return decode_wav_mono_16khz(audio);
    }
    decode_container_mono_16khz(audio, filename)
}

fn is_wav(audio: &[u8], filename: &str) -> bool {
    audio.starts_with(b"RIFF") && audio.get(8..12) == Some(b"WAVE")
        || filename
            .rsplit_once('.')
            .map(|(_, ext)| ext.eq_ignore_ascii_case("wav"))
            .unwrap_or(false)
}

fn decode_wav_mono_16khz(audio: &[u8]) -> Result<Vec<f32>, TranscriptionError> {
    let mut reader = hound::WavReader::new(Cursor::new(audio))
        .map_err(|error| TranscriptionError::Provider(format!("decode WAV audio: {error}")))?;
    let spec = reader.spec();
    if spec.channels == 0 || spec.channels > 8 {
        return Err(TranscriptionError::Provider(
            "WAV audio must contain between one and eight channels".into(),
        ));
    }
    if spec.sample_rate == 0 {
        return Err(TranscriptionError::Provider(
            "WAV audio has an invalid sample rate".into(),
        ));
    }
    let channels = usize::from(spec.channels);
    let mut mono = Vec::new();
    match (spec.sample_format, spec.bits_per_sample) {
        (hound::SampleFormat::Int, 8) => {
            for frame in reader
                .samples::<i16>()
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| {
                    TranscriptionError::Provider(format!("read WAV samples: {error}"))
                })?
                .chunks(channels)
            {
                mono.push(
                    frame
                        .iter()
                        .map(|sample| f32::from(*sample) / 128.0)
                        .sum::<f32>()
                        / channels as f32,
                );
            }
        }
        (hound::SampleFormat::Int, 16) => {
            for frame in reader
                .samples::<i16>()
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| {
                    TranscriptionError::Provider(format!("read WAV samples: {error}"))
                })?
                .chunks(channels)
            {
                mono.push(
                    frame
                        .iter()
                        .map(|sample| f32::from(*sample) / 32768.0)
                        .sum::<f32>()
                        / channels as f32,
                );
            }
        }
        (hound::SampleFormat::Int, 24) => {
            for frame in reader
                .samples::<i32>()
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| {
                    TranscriptionError::Provider(format!("read WAV samples: {error}"))
                })?
                .chunks(channels)
            {
                mono.push(
                    frame
                        .iter()
                        .map(|sample| *sample as f32 / 8_388_608.0)
                        .sum::<f32>()
                        / channels as f32,
                );
            }
        }
        (hound::SampleFormat::Int, 32) => {
            for frame in reader
                .samples::<i32>()
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| {
                    TranscriptionError::Provider(format!("read WAV samples: {error}"))
                })?
                .chunks(channels)
            {
                mono.push(
                    frame
                        .iter()
                        .map(|sample| *sample as f32 / 2_147_483_648.0)
                        .sum::<f32>()
                        / channels as f32,
                );
            }
        }
        (hound::SampleFormat::Float, 32) => {
            for frame in reader
                .samples::<f32>()
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| {
                    TranscriptionError::Provider(format!("read WAV samples: {error}"))
                })?
                .chunks(channels)
            {
                mono.push(frame.iter().copied().sum::<f32>() / channels as f32);
            }
        }
        _ => {
            return Err(TranscriptionError::Provider(format!(
                "unsupported WAV format: {:?} {}-bit",
                spec.sample_format, spec.bits_per_sample
            )))
        }
    }
    if mono.is_empty() {
        return Err(TranscriptionError::Provider(
            "WAV audio contains no samples".into(),
        ));
    }
    Ok(resample_linear(&mono, spec.sample_rate, 16_000))
}

fn decode_container_mono_16khz(
    audio: &[u8],
    filename: &str,
) -> Result<Vec<f32>, TranscriptionError> {
    let mut hint = Hint::new();
    if let Some((_, extension)) = filename.rsplit_once('.') {
        hint.with_extension(extension);
    }
    let source = Cursor::new(audio.to_vec());
    let media_source =
        MediaSourceStream::new(Box::new(source), MediaSourceStreamOptions::default());
    let format_options = FormatOptions {
        enable_gapless: true,
        ..FormatOptions::default()
    };
    let probed = get_probe()
        .format(
            &hint,
            media_source,
            &format_options,
            &MetadataOptions::default(),
        )
        .map_err(|error| format_decode_error("probe audio container", error))?;
    let mut format = probed.format;
    let track = format
        .default_track()
        .ok_or_else(|| TranscriptionError::Provider("audio has no default track".into()))?;
    let track_id = track.id;
    let sample_rate = track
        .codec_params
        .sample_rate
        .ok_or_else(|| TranscriptionError::Provider("audio has no sample rate".into()))?;
    let codec = track.codec_params.codec;
    let channels = track
        .codec_params
        .channels
        .map(|channels| channels.count())
        .ok_or_else(|| TranscriptionError::Provider("audio has no channel layout".into()))?;
    if channels == 0 || channels > 2 {
        return Err(TranscriptionError::Provider(
            "audio must contain one or two channels".into(),
        ));
    }
    if codec == CODEC_TYPE_OPUS {
        return decode_opus_container(format.as_mut(), track_id, channels);
    }
    let mut decoder = get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(|error| format_decode_error("create audio decoder", error))?;
    let mut mono = Vec::new();
    loop {
        let packet = match format.next_packet() {
            Ok(packet) if packet.track_id() == track_id => packet,
            Ok(_) => continue,
            Err(SymphoniaError::ResetRequired) => {
                return Err(TranscriptionError::Provider(
                    "audio decoder requires a reset".into(),
                ))
            }
            Err(SymphoniaError::IoError(error))
                if error.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                break
            }
            Err(SymphoniaError::DecodeError(_)) => continue,
            Err(error) => return Err(format_decode_error("read audio packet", error)),
        };
        let decoded = decoder
            .decode(&packet)
            .map_err(|error| format_decode_error("decode audio packet", error))?;
        append_mono(&mut mono, decoded);
    }
    if mono.is_empty() {
        return Err(TranscriptionError::Provider(
            "audio contains no decodable samples".into(),
        ));
    }
    Ok(resample_linear(&mono, sample_rate, 16_000))
}

fn decode_opus_container(
    format: &mut dyn FormatReader,
    track_id: u32,
    channels: usize,
) -> Result<Vec<f32>, TranscriptionError> {
    let opus_channels = match channels {
        1 => OpusChannels::Mono,
        2 => OpusChannels::Stereo,
        _ => unreachable!("validated Opus channel count"),
    };
    // Opus is decoded at its native 48 kHz rate. The caller performs the
    // final resample after container decoding.
    let mut decoder = OpusDecoder::new(48_000, opus_channels)
        .map_err(|error| TranscriptionError::Provider(format!("create Opus decoder: {error}")))?;
    let mut mono = Vec::new();
    loop {
        let packet = match format.next_packet() {
            Ok(packet) if packet.track_id() == track_id => packet,
            Ok(_) => continue,
            Err(SymphoniaError::ResetRequired) => {
                return Err(TranscriptionError::Provider(
                    "audio decoder requires a reset".into(),
                ))
            }
            Err(SymphoniaError::IoError(error))
                if error.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                break
            }
            Err(error) => return Err(format_decode_error("read Opus packet", error)),
        };
        let mut decoded = vec![0.0_f32; 5_760 * channels];
        let frame_samples = decoder
            .decode_float(&packet.data, &mut decoded, false)
            .map_err(|error| {
                TranscriptionError::Provider(format!("decode Opus packet: {error}"))
            })?;
        let start = usize::try_from(packet.trim_start).unwrap_or(usize::MAX);
        let end_trim = usize::try_from(packet.trim_end).unwrap_or(usize::MAX);
        if start.saturating_add(end_trim) >= frame_samples {
            continue;
        }
        let end = frame_samples - end_trim;
        for frame in decoded[start * channels..end * channels].chunks_exact(channels) {
            mono.push(frame.iter().copied().sum::<f32>() / channels as f32);
        }
    }
    if mono.is_empty() {
        return Err(TranscriptionError::Provider(
            "audio contains no decodable samples".into(),
        ));
    }
    Ok(resample_linear(&mono, 48_000, 16_000))
}

fn append_mono(output: &mut Vec<f32>, audio: AudioBufferRef<'_>) {
    let spec = *audio.spec();
    let channels = spec.channels.count();
    if channels == 0 {
        return;
    }
    let mut samples = SampleBuffer::<f32>::new(audio.capacity() as u64, spec);
    samples.copy_interleaved_ref(audio);
    output.extend(
        samples
            .samples()
            .chunks(channels)
            .map(|frame| frame.iter().copied().sum::<f32>() / channels as f32),
    );
}

fn format_decode_error(context: &str, error: SymphoniaError) -> TranscriptionError {
    TranscriptionError::Provider(format!("{context}: {error}"))
}

fn resample_linear(samples: &[f32], from_rate: u32, to_rate: u32) -> Vec<f32> {
    if from_rate == to_rate || samples.len() < 2 {
        return samples.to_vec();
    }
    let output_len = ((samples.len() as u64 * u64::from(to_rate)) / u64::from(from_rate)) as usize;
    (0..output_len.max(1))
        .map(|index| {
            let position = index as f64 * f64::from(from_rate) / f64::from(to_rate);
            let left = position.floor() as usize;
            let right = (left + 1).min(samples.len() - 1);
            let fraction = (position - left as f64) as f32;
            samples[left.min(samples.len() - 1)] * (1.0 - fraction) + samples[right] * fraction
        })
        .collect()
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
    fn native_decoder_mixes_stereo_wav_without_shelling_out() {
        let mut cursor = Cursor::new(Vec::new());
        let spec = hound::WavSpec {
            channels: 2,
            sample_rate: 16_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        {
            let mut writer = hound::WavWriter::new(&mut cursor, spec).unwrap();
            writer.write_sample(i16::MAX).unwrap();
            writer.write_sample(i16::MIN).unwrap();
            writer.write_sample(16_384).unwrap();
            writer.write_sample(16_384).unwrap();
            writer.finalize().unwrap();
        }

        let samples = decode_wav_mono_16khz(&cursor.into_inner()).unwrap();
        assert_eq!(samples.len(), 2);
        assert!(samples[0].abs() < 0.0001);
        assert!((samples[1] - 0.5).abs() < 0.0001);
    }

    #[test]
    fn native_decoder_resamples_to_whisper_rate() {
        let samples = resample_linear(&[0.0, 1.0], 8_000, 16_000);
        assert_eq!(samples.len(), 4);
        assert_eq!(samples, [0.0, 0.5, 1.0, 1.0]);
    }

    #[test]
    fn native_decoder_rejects_invalid_audio_and_missing_model() {
        let error = decode_wav_mono_16khz(b"not a wav").unwrap_err();
        assert!(error.to_string().contains("decode WAV"));

        let directory = tempfile::tempdir().unwrap();
        let error = WhisperTranscriber::new(directory.path().join("missing.bin"), "whisper-1", 1)
            .unwrap_err();
        assert!(error.to_string().contains("does not exist"));
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
