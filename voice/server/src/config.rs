use std::{net::SocketAddr, path::PathBuf};

use clap::Parser;

#[derive(Debug, Clone, Parser)]
#[command(name = "vogt-voice", version, about = "Vogt's optional voice sidecar")]
pub struct Config {
    /// HTTP listen address. The sidecar is normally reached only over the
    /// internal Compose network.
    #[arg(long, env = "VOGT_VOICE_BIND", default_value = "0.0.0.0:8000")]
    pub bind: SocketAddr,

    /// Directory reserved for downloaded model weights by future backends.
    #[arg(
        long,
        env = "VOGT_VOICE_MODEL_CACHE_DIR",
        default_value = "/var/cache/vogt-voice"
    )]
    pub model_cache_dir: PathBuf,

    /// Advertised STT model id. This does not enable inference by itself.
    #[arg(long, env = "VOGT_VOICE_STT_MODEL", default_value = "whisper-1")]
    pub stt_model: String,

    /// Advertised TTS model id. This does not enable inference by itself.
    #[arg(long, env = "VOGT_VOICE_TTS_MODEL", default_value = "tts-1")]
    pub tts_model: String,

    /// Advertised default TTS voice.
    #[arg(long, env = "VOGT_VOICE_TTS_VOICE", default_value = "alloy")]
    pub tts_voice: String,

    /// JSON argv for the optional STT executable. `{audio}`, `{model}`,
    /// `{language}`, and `{prompt}` are available placeholders. There is no
    /// default: an absent command leaves STT explicitly unconfigured.
    #[arg(long, env = "VOGT_VOICE_STT_COMMAND", value_parser = parse_command)]
    pub stt_command: Option<String>,

    /// JSON argv for the optional TTS executable. `{model}`, `{voice}`, and
    /// `{format}` are available placeholders. The request text is sent on
    /// stdin and audio is read from stdout.
    #[arg(long, env = "VOGT_VOICE_TTS_COMMAND", value_parser = parse_command)]
    pub tts_command: Option<String>,

    /// Maximum runtime for one configured provider process.
    #[arg(long, env = "VOGT_VOICE_COMMAND_TIMEOUT_MS", default_value = "120000")]
    pub command_timeout_ms: u64,
}

fn parse_command(value: &str) -> Result<String, String> {
    let command: Vec<String> = serde_json::from_str(value)
        .map_err(|error| format!("voice command must be a JSON argv array: {error}"))?;
    if command.is_empty() || command[0].trim().is_empty() {
        return Err("voice command must contain an executable".into());
    }
    if command.iter().any(|arg| arg.contains('\0')) {
        return Err("voice command must not contain NUL bytes".into());
    }
    Ok(value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn commands_are_explicit_json_argv() {
        let config = Config::parse_from([
            "vogt-voice",
            "--stt-command",
            r#"["whisper","{audio}"]"#,
            "--tts-command",
            r#"["piper","--voice","{voice}"]"#,
        ]);
        assert_eq!(config.stt_command.unwrap(), r#"["whisper","{audio}"]"#);
        assert_eq!(
            config.tts_command.unwrap(),
            r#"["piper","--voice","{voice}"]"#
        );
    }

    #[test]
    fn malformed_command_is_rejected() {
        assert!(parse_command("whisper").is_err());
        assert!(parse_command("[]").is_err());
        assert!(parse_command("[\"whisper\\u0000\"]").is_err());
    }
}
