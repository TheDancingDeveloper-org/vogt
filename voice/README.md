# Vogt voice sidecar

`vogt-voice` is an optional Rust service for issue #326. It speaks
the OpenAI-compatible sidecar contract used by the engine:

- `GET /health`
- `GET /v1/models`
- `POST /v1/audio/transcriptions` with multipart `file` and `model`
- `POST /v1/audio/speech` with JSON `{model, voice, input, response_format}`

The default STT and TTS providers remain explicit `unconfigured`
implementations, so audio requests return a structured HTTP `501` response.
For a usable first-party deployment, configure either half with the optional
subprocess backend. It runs a real locally installed inference executable and
does not synthesize placeholder text or bytes:

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
invocation, or model weight in the image by default. The operator must build
or mount the chosen runtime and model files, and should run the sidecar on an
internal network as the existing `deploy/voice.overlay.yml` does. A command
can be configured for only one half; the other remains unconfigured. The
existing third-party overlay is unchanged and remains the supported
turn-key local stack.

Run from this directory:

```bash
cargo fmt --check
cargo test --all
cargo clippy --all-targets --all-features -- -D warnings
cargo run -p vogt-voice-server
```

No first-party deployment overlay is included: model runtimes and weights are
deployment-specific. The existing `deploy/voice.overlay.yml` remains
unchanged and continues to provide the third-party local providers.
