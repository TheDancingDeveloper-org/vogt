# Vogt voice sidecar

`vogt-voice` is an optional Rust service for issue #326. It speaks
the OpenAI-compatible sidecar contract used by the engine:

- `GET /health`
- `GET /v1/models`
- `POST /v1/audio/transcriptions` with multipart `file` and `model`
- `POST /v1/audio/speech` with JSON `{model, voice, input, response_format}`

The default STT and TTS providers remain explicit `unconfigured`
implementations, so audio requests return a structured HTTP `501` response.
For a first-party deployment, mount real model files and configure the native
providers:

```bash
export VOGT_VOICE_STT_MODEL_PATH=/models/ggml-base.en.bin
export VOGT_VOICE_TTS_MODEL_CONFIG_PATH=/models/en_US-lessac-medium.onnx.json
export VOGT_VOICE_STT_MODEL=whisper-1
export VOGT_VOICE_TTS_MODEL=tts-1
export VOGT_VOICE_TTS_VOICE=alloy       # API alias for the mounted Piper voice
export VOGT_VOICE_THREADS=2
```

The native STT backend is `whisper-rs` (Whisper.cpp/GGML) and accepts WAV PCM,
WebM/Opus, Ogg/Opus, and Ogg/Vorbis audio, mixing and resampling it to mono
16 kHz before inference. The native TTS backend is `piper-rs` over ONNX and
returns PCM WAV. Piper's JSON configuration
must sit beside its ONNX file using Piper's normal naming convention (for
example `voice.onnx` and `voice.onnx.json`). The model files are deliberately
not downloaded or baked into the image: put them in an operator-managed model
cache and mount that cache read-only. When a native model path is configured,
`/health` returns `503` until that model has loaded, so Compose does not start
the engine against a broken model mount. No audio is retained; Piper's response
is written to a temporary file and read back before that file is deleted.

Native Piper currently supports `wav` responses and OpenAI-compatible `speed`
values from `0.25` through `4.0`. The mounted Piper model fixes the actual
voice; `VOGT_VOICE_TTS_VOICE` is the API name clients must send. Unsupported
formats and invalid speeds are rejected rather than mislabeled.

If native model files are not available, either half can instead use the
optional subprocess backend. It runs a real locally installed inference
executable and does not synthesize placeholder text or bytes:

```bash
export VOGT_VOICE_STT_COMMAND='["whisper-cli","--model","/models/ggml-base.en.bin","--file","{audio}"]'
export VOGT_VOICE_TTS_COMMAND='["my-piper-wrapper","--model","/models/en_US-lessac-medium.onnx","--voice","{voice}","--format","{format}"]'
export VOGT_VOICE_COMMAND_TIMEOUT_MS=120000
```

The command values are JSON argv arrays, not shell strings. STT receives the
uploaded audio in a temporary file whose path is substituted for `{audio}`
(or appended when the placeholder is absent), and must print only the UTF-8
transcript to stdout. `{model}`, `{language}`, and `{prompt}` are also
available. TTS receives the input text on stdin, and must write the requested
audio encoding to stdout; `{model}`, `{voice}`, and `{format}` are available.
The process is killed after the configured timeout. Stderr is used only for a
bounded failure detail and audio is not retained.

There is deliberately no executable, model path, model download, shell
invocation, or model weight in the image by default. The operator must mount
the model cache and should run the sidecar on an internal network. A native
model path takes precedence over a subprocess command for that half; either
half can be configured independently. The first-party deployment is
`deploy/voice.firstparty.overlay.yml`. The existing third-party
`deploy/voice.overlay.yml` remains available as an alternative.

Run from this directory:

```bash
cargo fmt --check
cargo test --all
cargo clippy --all-targets --all-features -- -D warnings
cargo run -p vogt-voice-server
```

The development host needs `clang`, `cmake`, `libclang-dev`, `libopus-dev`,
and `pkg-config` for the native bindings (`apt install clang cmake
libclang-dev libopus-dev pkg-config` on Debian/Ubuntu). The published image
installs these build dependencies in its build stage and the required runtime
libraries in its own runtime stage.

The Docker image builds the native runtimes with Rust 1.80 and installs the
small runtime library required by ONNX Runtime. `cargo test --workspace` does
not require model weights; model-backed smoke testing requires the operator's
mounted Whisper and Piper files.
