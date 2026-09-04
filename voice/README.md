# Vogt voice sidecar

`vogt-voice` is Vogt's first-party voice sidecar (#326, #565). It is the
`voice` service in [`deploy/stack.compose.yml`](../deploy/stack.compose.yml),
on by default in the shipped stack (`COMPOSE_PROFILES=voice`), published and
signed by the same release as the stack image and versioned with it. It speaks
the OpenAI-compatible sidecar contract the engine uses:

- `GET /health`
- `GET /v1/models`
- `POST /v1/audio/transcriptions` with multipart `file` and `model`
- `POST /v1/audio/speech` with JSON `{model, voice, input, response_format}`

## What the published image does out of the box

The image bakes in a small, permissively-licensed default model set — Whisper
`base.en` (GGML, MIT) and the public-domain Piper English voice
`en_US-ljspeech-medium` — fetched at build from pinned Hugging Face revisions
and verified by SHA-256. `VOGT_VOICE_STT_MODEL_PATH` and
`VOGT_VOICE_TTS_MODEL_CONFIG_PATH` default to those baked files, so the
sidecar transcribes and speaks with no operator input. The release build
gates on a full round trip through them — synthesise a phrase, feed the audio
back, read the transcript — so a mute sidecar cannot publish.

The advertised API names are `whisper-1` (STT), `tts-1` (TTS) and `alloy`
(the one voice), which is what `stack.compose.yml` tells the engine to send.
`/health` reports `starting` until the models have loaded, then `ok`; Compose
waits on it before it considers the stack ready.

The native STT backend is `whisper-rs` (Whisper.cpp/GGML) and accepts WAV
PCM, WebM/Opus, Ogg/Opus, and Ogg/Vorbis audio, mixing and resampling it to
mono 16 kHz before inference. The native TTS backend is `piper-rs` over ONNX
and returns PCM **WAV only** — which is why the stack sets
`ENGINE_ASSISTANT_TTS_FORMAT=wav`; the engine streams the upstream content
type through, so it plays in the PWA. OpenAI-compatible `speed` values from
`0.25` through `4.0` are honoured. Unsupported formats and invalid speeds are
rejected rather than mislabeled. No audio is retained: Piper's response is
written to a temporary file and read back before that file is deleted.

## Bring your own models

The baked defaults are a starting point, not a lock-in. Either half can be
pointed at models of your own, independently:

```bash
export VOGT_VOICE_STT_MODEL_PATH=/models/ggml-base.en.bin
export VOGT_VOICE_TTS_MODEL_CONFIG_PATH=/models/en_US-lessac-medium.onnx.json
export VOGT_VOICE_STT_MODEL=whisper-1
export VOGT_VOICE_TTS_MODEL=tts-1
export VOGT_VOICE_TTS_VOICE=alloy       # the API name clients send; the model fixes the actual voice
export VOGT_VOICE_THREADS=2
```

Build an image that starts `FROM` the published sidecar and overrides these,
or mount a read-only model cache and set the paths. Piper's JSON configuration
must sit beside its ONNX file under Piper's normal naming (for example
`voice.onnx` and `voice.onnx.json`). When a native model path is configured,
`/health` returns `503` until that model has loaded, so a broken model mount
keeps the sidecar unhealthy rather than serving — the engine is never
started against it.

**Explicitly unconfigured is still a state.** If a half has neither a native
model path nor a subprocess command, it answers audio requests with a
structured HTTP `501`. The sidecar never synthesises placeholder text or
bytes to look alive; that is the behaviour the baked defaults sit in front
of, not a replacement for it.

## Subprocess backend

If native model files are not what you have, either half can instead run a
real, locally installed inference executable:

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
bounded failure detail and audio is not retained. A native model path takes
precedence over a subprocess command for the same half.

Run the sidecar on an internal network only; `stack.compose.yml` never
publishes it to the host — the engine stays the only front door.

## Developing it

Run from this directory:

```bash
cargo fmt --check
cargo test --all
cargo clippy --all-targets --all-features -- -D warnings
cargo run -p vogt-voice-server
```

The development host needs `clang`, `cmake`, `libclang-dev`, `libopus-dev`,
and `pkg-config` for the native bindings (`apt install clang cmake
libclang-dev libopus-dev pkg-config` on Debian/Ubuntu). `cargo test
--workspace` does not require model weights; the model-backed round trip is
the image build's own gate.

The `Dockerfile` builds the native runtimes on the estate-mirrored
`rust:1-bookworm` base, fetches and verifies the default models in a stage of
their own (so the ~210 MB download is a cache layer that only changes when a
pinned revision does), and installs the ONNX Runtime's one runtime library
into a minimal `ubuntu:26.04` final stage.
