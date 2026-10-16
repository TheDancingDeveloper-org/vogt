# syntax=docker/dockerfile:1
#
# The Rust core as a drop-in for the Python core image.
#
# Same contract as the root Dockerfile's runtime: user 1000:0, data at
# /var/lib/vogt, entrypoint the lifecycle runner, and a `vogt` binary on PATH
# that answers `init` and `serve`. The compose files call `vogt`, not
# `vogt-core`, so the binary is installed under both names.
#
# Build context is the repository root:
#
#   docker build -t vogt-core:rust -f deploy/rust-core.Dockerfile .
#
# It is the core slot of `deploy/vogt.compose.yml`. The engine image still
# copies the Python core for its own loopback half; this image is the
# standalone core the contributor stack fronts.

ARG RUST_IMAGE=rust:1-bookworm
ARG DEBIAN_IMAGE=debian:bookworm-slim

FROM ${RUST_IMAGE} AS build
WORKDIR /src
# The core crate lives in the engine workspace so `cargo test -p vogt-core`
# works from engine/. This image builds it alone: the workspace also contains
# the engine server, which this image does not ship and should not compile.
COPY engine/core core
# embedded.rs includes the SQL five directories above the core crate, which
# from engine/core is the repository's src/vogt. This image has no engine/
# directory, so the crate sits at core/ and the SQL has to sit five up.
COPY src/vogt/storage/sqlite/migrations /src/vogt/storage/sqlite/migrations
RUN printf '%s\n' \
      '[workspace]' \
      'members = ["core"]' \
      'resolver = "2"' \
      '' \
      '[workspace.package]' \
      'edition = "2021"' \
      'license = "AGPL-3.0-only"' \
      'repository = "https://github.com/TheDancingDeveloper-org/vogt"' \
      'rust-version = "1.94"' \
      > Cargo.toml
WORKDIR /src
RUN cargo build --release -p vogt-core

FROM ${DEBIAN_IMAGE} AS runtime

LABEL org.opencontainers.image.title="vogt" \
      org.opencontainers.image.description="Vogt core, the Rust build" \
      org.opencontainers.image.source="https://github.com/TheDancingDeveloper-org/vogt" \
      org.opencontainers.image.licenses="AGPL-3.0-only"

RUN apt-get update \
    && apt-get install -y --no-install-recommends git ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --gid 1000 vogt \
    && useradd --uid 1000 --gid 0 --no-create-home --shell /usr/sbin/nologin vogt \
    && mkdir -p /var/lib/vogt \
    && chown root:0 /var/lib/vogt \
    && chmod 0770 /var/lib/vogt

COPY --from=build /src/target/release/vogt-core /usr/local/bin/vogt-core
RUN ln -s vogt-core /usr/local/bin/vogt
COPY --chmod=755 vogt-lifecycle.sh /usr/local/bin/vogt-lifecycle

ENV VOGT_DATA_DIR=/var/lib/vogt
VOLUME ["/var/lib/vogt"]
USER 1000:0
ENTRYPOINT ["/usr/local/bin/vogt-lifecycle"]
CMD ["--help"]
