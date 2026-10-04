# Vogt voice sidecar

`vogt-voice` is Vogt's first-party voice sidecar. It is the
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
`0.25` through `4.0` are honoured by dividing the voice's own `length_scale`
(its phoneme-duration multiplier), so `2.0` speaks in half the time. Unsupported
formats and invalid speeds are rejected rather than mislabeled. No audio is
retained: Piper's samples are encoded to WAV in memory and never touch disk.

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
`voice.onnx` and `voice.onnx.json`). Only single-file voices are supported:
a config declaring Piper's streaming encoder/decoder layout (`"streaming":
true`) is refused at load, since `piper-rs` 0.2 no longer runs it. When a
native model path is configured,
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
`libssl-dev` and `pkg-config` for the native bindings (`apt install clang cmake
libclang-dev libopus-dev libssl-dev pkg-config` on Debian/Ubuntu); OpenSSL is
for `ort-sys`'s build script, which downloads the ONNX Runtime over native TLS.
`cargo test --workspace` does not require model weights; the model-backed round
trip is the image build's own gate. To run one locally, fetch the pinned voice
from the `Dockerfile` and run the ignored test:

```bash
VOGT_VOICE_TEST_PIPER_CONFIG=/path/to/en_US-ljspeech-medium.onnx.json \
PIPER_ESPEAKNG_DATA_DIRECTORY="$(dirname "$(find /tmp/vt/debug/build -type d -path '*/out/share/espeak-ng-data' -print -quit)")" \
CARGO_TARGET_DIR=/tmp/vt cargo test -p vogt-voice-tts -- --ignored
```

Keep the target directory's path short. `espeak-rs-sys` compiles espeak-ng's
phoneme data in its build script, and espeak-ng builds each source path in a
fixed buffer (160 bytes for the data directory plus a few dozen for the file
name), silently truncating longer ones. With the checkout deep in the file
system, such as a worktree under `.claude/worktrees/`, the build directory
`target/debug/build/espeak-rs-sys-<hash>/out/build/espeak-ng-data` passes that
limit and the build fails with `Failed to open: '…/phsource/vowel/oo_e'` (the
real file is `oo_en`) or `Error processing file '…/phsource/intonation'`.
CI and the image build from short paths. Locally, point Cargo at a short
target directory:

```bash
CARGO_TARGET_DIR=/tmp/vt cargo build -p vogt-voice-tts
```

The `Dockerfile` builds the native runtimes on the estate-mirrored
`rust:1-trixie` base (glibc 2.41: the ONNX Runtime static library `ort-sys`
downloads needs glibc >= 2.38, which bookworm's 2.36 is not), fetches and verifies the default models in a stage of
their own (so the ~210 MB download is a cache layer that only changes when a
pinned revision does), and copies the binary, the compiled espeak-ng data and
the models into a minimal `ubuntu:26.04` final stage. The ONNX Runtime is linked
statically into the binary, so the final stage carries no runtime library for
it.

### Dependency pins

`tts/Cargo.toml` pins `ort` and `ort-sys` at `=2.0.0-rc.12` and `piper-rs` at
`=0.2.0`, which requires exactly that `ort`. `ort` and `ort-sys` must stay on
the same release candidate (a lone `ort-sys` bump fails its own build script),
so Dependabot ignores both, and they move by hand together with `piper-rs`.
`piper-rs` uses `ort`'s default features, which include `tls-native`, so a
rustls-only build is not available without forking it.

Check for API changes when bumping `piper-rs`. 0.2 replaced
`from_config_path`, the `synth` module and `synthesize_to_file` with
`Piper::create`, which returns raw f32 samples through `&mut self`; `tts/`
therefore builds the ONNX session itself, holds the voice behind a mutex,
encodes WAV with `hound`, and maps `speed` onto `length_scale`.

STT container decoding uses `symphonia` 0.6. Its WebM/Opus, Ogg/Opus and
Ogg/Vorbis paths are pinned by the fixtures in `stt/tests/fixtures/` (0.2 s
test tones generated with GStreamer's `audiotestsrc`). The WAV-only tests do
not reach that code, so keep those fixtures when changing the decoder.

The declared `rust-version = "1.88"` is the floor the lockfile actually needs
(`whisper-rs-sys`, `icu_*`, `home`); every build, in CI and in the image,
uses current stable. CI's `scripts/check_rust_version.py` fails when a locked
crate declares a newer `rust-version` than the workspace does, so a dependency
bump that raises the floor must raise the declaration with it. There are no
pins held back only for an older toolchain.
