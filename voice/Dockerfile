# Optional first-party voice sidecar.
# Inference executables and model weights are intentionally not included: use
# VOGT_VOICE_*_COMMAND with an operator-built image or mounted runtime.
FROM rust:1.80-bookworm AS build
WORKDIR /src
COPY voice/Cargo.toml voice/Cargo.lock* ./voice/
COPY voice/models ./voice/models
COPY voice/server ./voice/server
COPY voice/stt ./voice/stt
COPY voice/tts ./voice/tts
RUN cargo build --locked --release --manifest-path voice/Cargo.toml -p vogt-voice-server

FROM debian:bookworm-slim
RUN useradd --system --create-home --uid 10001 vogt \
    && mkdir -p /var/cache/vogt-voice \
    && chown -R vogt:vogt /var/cache/vogt-voice
COPY --from=build /src/voice/target/release/vogt-voice /usr/local/bin/vogt-voice
USER vogt
ENV VOGT_VOICE_BIND=0.0.0.0:8000 \
    VOGT_VOICE_MODEL_CACHE_DIR=/var/cache/vogt-voice
EXPOSE 8000
ENTRYPOINT ["/usr/local/bin/vogt-voice"]
