# Vogt voice sidecar

`vogt-voice` is an optional Rust service foundation for issue #326. It speaks
the OpenAI-compatible sidecar contract used by the engine:

- `GET /health`
- `GET /v1/models`
- `POST /v1/audio/transcriptions` with multipart `file` and `model`
- `POST /v1/audio/speech` with JSON `{model, voice, input, response_format}`

The service is deliberately not an inference implementation yet. Its default
STT and TTS providers are explicit `unconfigured` implementations, so audio
requests return a structured HTTP `501` response. This is safer than exposing
a route that silently returns fake or empty audio. The `stt/` and `tts/`
crates define the provider traits where a future Whisper/Piper/Candle/ONNX
backend can be installed, and `models/` defines the model catalog/cache
boundary without downloading weights.

Run from this directory:

```bash
cargo fmt --check
cargo test --all
cargo clippy --all-targets --all-features -- -D warnings
cargo run -p vogt-voice-server
```

No deployment overlay is included in this foundation. The existing
`deploy/voice.overlay.yml` remains unchanged and continues to provide the
currently supported third-party local providers.
