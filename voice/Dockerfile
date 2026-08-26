# Optional first-party voice sidecar. The binary contains native Whisper and
# Piper/ONNX inference; model weights remain deployment-owned mounts.
FROM rust:1.80-bookworm AS build
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

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install --no-install-recommends -y curl libgomp1 libopus0 \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --create-home --uid 10001 vogt \
    && mkdir -p /var/cache/vogt-voice \
    && chown -R vogt:vogt /var/cache/vogt-voice
COPY --from=build /src/target/release/vogt-voice /usr/local/bin/vogt-voice
COPY --from=build /usr/local/share/espeak-ng-data /usr/local/share/espeak-ng-data
USER vogt
ENV VOGT_VOICE_BIND=0.0.0.0:8000 \
    VOGT_VOICE_MODEL_CACHE_DIR=/var/cache/vogt-voice \
    PIPER_ESPEAKNG_DATA_DIRECTORY=/usr/local/share
EXPOSE 8000
ENTRYPOINT ["/usr/local/bin/vogt-voice"]
