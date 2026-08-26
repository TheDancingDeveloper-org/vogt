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
}
