# syntax=docker/dockerfile:1
#
# The first-party voice sidecar, `vogt-voice` (#326, #565).
#
# One binary containing native Whisper (GGML) and Piper (ONNX) inference, plus
# a small, permissively-licensed default model set baked in so a fresh
# `deploy/stack.compose.yml` has working speech with no operator input. An
# operator who wants their own weights points the model-path env vars below at
# a mount of their own; the "explicit-unconfigured → 501" contract in the
# server is unchanged.
#
# This is built by CI only (`.github/workflows/build.yml`, `release.yml`) and
# published as a signed image — nothing under `deploy/` builds it. Like the
# engine image, its bases are named through this organisation's GHCR mirror
# (`ghcr.io/thedancingdeveloper-org/vogt-base/*`) rather than Docker Hub, so a
# busy self-hosted runner's shared anonymous pull limit is not on the build's
# critical path (#33). `mirror-base-images.yml` guards that these stay mirror
# refs. The `rust:1-bookworm` and `ubuntu:26.04` tags are already mirrored for
# the engine and pod-base builds, so this file adds no new mirrored base.

# ─── build: the Rust binary with native inference runtimes ──────────────────
FROM ghcr.io/thedancingdeveloper-org/vogt-base/rust:1-bookworm AS build
RUN apt-get update \
    && apt-get install --no-install-recommends -y clang cmake libclang-dev libopus-dev pkg-config \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY Cargo.toml Cargo.lock* ./
COPY models ./models
COPY server ./server
COPY stt ./stt
COPY tts ./tts
RUN cargo build --locked --release --manifest-path Cargo.toml -p vogt-voice-server
RUN espeak_data_dir="$(find target/release/build -type d -path '*/out/share/espeak-ng-data' -print -quit)" \
    && test -n "$espeak_data_dir" \
    && mkdir -p /usr/local/share \
    && cp -a "$espeak_data_dir" /usr/local/share/espeak-ng-data

# ─── models: the baked default weights, verified by digest ──────────────────
#
# Kept a stage of its own so the ~210 MB download is a cache layer that only
# rebuilds when a pin below changes, not on every source edit. Every file is
# fetched from a pinned Hugging Face revision and checked against its SHA-256:
# a moved or truncated weight fails the build rather than shipping quietly. The
# defaults are deliberately small and permissively licensed —
#   * STT: Whisper `base.en` GGML (ggerganov/whisper.cpp, MIT).
#   * TTS: Piper `en_US-ljspeech-medium` (LJ Speech, public domain).
# so redistribution inside a published image is clean.
FROM ghcr.io/thedancingdeveloper-org/vogt-base/ubuntu:26.04 AS models
RUN apt-get update \
    && apt-get install --no-install-recommends -y curl ca-certificates \
    && rm -rf /var/lib/apt/lists/*
# Pinned Hugging Face revisions — a commit, never a moving branch (NFR-D10 in
# spirit: the bytes cannot change under a fixed reference).
ARG WHISPER_REV=5359861c739e955e79d9a303bcbc70fb988958b1
ARG PIPER_REV=39ab474be869e9181350af6a65e4953eef67aaa0
ARG WHISPER_SHA256=a03779c86df3323075f5e796cb2ce5029f00ec8869eee3fdfb897afe36c6d002
ARG PIPER_ONNX_SHA256=6f52a751e2349abe7a76735eb09dc1875298c77ea2342ffd2fef79ff81b87f22
ARG PIPER_JSON_SHA256=141d612cc0a95ed7efc1ca936b845c2364967f2e9217c5dbfcf69fc4d6c65860
WORKDIR /models
RUN set -eux; \
    hf="https://huggingface.co"; \
    curl -fSL --retry 3 --retry-delay 5 \
        -o ggml-base.en.bin \
        "${hf}/ggerganov/whisper.cpp/resolve/${WHISPER_REV}/ggml-base.en.bin"; \
    echo "${WHISPER_SHA256}  ggml-base.en.bin" | sha256sum -c -; \
    piper="${hf}/rhasspy/piper-voices/resolve/${PIPER_REV}/en/en_US/ljspeech/medium"; \
    curl -fSL --retry 3 --retry-delay 5 \
        -o en_US-ljspeech-medium.onnx "${piper}/en_US-ljspeech-medium.onnx"; \
    echo "${PIPER_ONNX_SHA256}  en_US-ljspeech-medium.onnx" | sha256sum -c -; \
    curl -fSL --retry 3 --retry-delay 5 \
        -o en_US-ljspeech-medium.onnx.json "${piper}/en_US-ljspeech-medium.onnx.json"; \
    echo "${PIPER_JSON_SHA256}  en_US-ljspeech-medium.onnx.json" | sha256sum -c -

# ─── runtime ────────────────────────────────────────────────────────────────
FROM ghcr.io/thedancingdeveloper-org/vogt-base/ubuntu:26.04
LABEL org.opencontainers.image.title="vogt-voice" \
      org.opencontainers.image.description="Vogt's first-party voice sidecar (STT + TTS)" \
      org.opencontainers.image.source="https://github.com/TheDancingDeveloper-org/vogt" \
      org.opencontainers.image.licenses="MIT"
RUN apt-get update \
    && apt-get install --no-install-recommends -y curl ca-certificates libgomp1 libopus0 \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --create-home --uid 10001 vogt \
    && mkdir -p /var/cache/vogt-voice \
    && chown -R vogt:vogt /var/cache/vogt-voice
COPY --from=build /src/target/release/vogt-voice /usr/local/bin/vogt-voice
COPY --from=build /usr/local/share/espeak-ng-data /usr/local/share/espeak-ng-data
# The baked defaults, owned by root and left read-only: the sidecar only ever
# reads them, and audio is written to tmpfs, so nothing needs write access here.
COPY --from=models /models /opt/vogt-voice/models
RUN chmod -R a-w /opt/vogt-voice/models
USER vogt
ENV VOGT_VOICE_BIND=0.0.0.0:8000 \
    VOGT_VOICE_MODEL_CACHE_DIR=/var/cache/vogt-voice \
    PIPER_ESPEAKNG_DATA_DIRECTORY=/usr/local/share \
    VOGT_VOICE_STT_MODEL_PATH=/opt/vogt-voice/models/ggml-base.en.bin \
    VOGT_VOICE_TTS_MODEL_CONFIG_PATH=/opt/vogt-voice/models/en_US-ljspeech-medium.onnx.json
EXPOSE 8000
# `/health` gates on the baked models loading, so this reports "starting" until
# Whisper and Piper are in memory and "healthy" once a request can be served.
HEALTHCHECK --interval=30s --timeout=10s --start-period=90s --retries=3 \
    CMD curl -fsS http://127.0.0.1:8000/health || exit 1
ENTRYPOINT ["/usr/local/bin/vogt-voice"]
