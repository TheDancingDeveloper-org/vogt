# syntax=docker/dockerfile:1
#
# The PWA bundle on its own, for a core-only deployment (WI-2641).
#
# The shipped product embeds `web/dist` in the engine binary (rust-embed in
# `engine/server/src/assets.rs`), so there is no web client without an engine.
# The rust-core preview runs the Rust core with no engine at all, and something
# else has to serve the bundle and stand in for the engine's front door. This
# image only carries the files: the preview stack copies `/srv/web` into a
# volume a stock web server reads. It serves nothing itself.
#
# Built by `.github/workflows/build.yml` (`preview-web-image`) on a dispatch of
# the `rust-core` ref only. Build context is the repository root:
#
#   docker build -t vogt-web:preview -f deploy/rust-core-preview-web.Dockerfile .

# The same mirrored bases engine/Dockerfile uses, so this build never pulls
# from Docker Hub.
ARG NODE_IMAGE=ghcr.io/thedancingdeveloper-org/vogt-base/node:22-bookworm
ARG RUNTIME_IMAGE=ghcr.io/thedancingdeveloper-org/vogt-base/node:22-bookworm-slim
ARG VOGT_SOURCE_REF=local
ARG VOGT_SOURCE_SHA=0000000000000000000000000000000000000000
ARG VOGT_PRODUCT_VERSION=local/dev

# The same steps as engine/Dockerfile's `web-build` stage, so the bundle is the
# one the engine would embed for this commit.
FROM ${NODE_IMAGE} AS web-build
ARG VOGT_SOURCE_REF
ARG VOGT_SOURCE_SHA
ARG VOGT_PRODUCT_VERSION
WORKDIR /app/web
COPY web/package.json web/pnpm-workspace.yaml web/pnpm-lock.yaml ./
RUN npm install -g pnpm && pnpm install --frozen-lockfile
COPY web/ ./
RUN set -eu; \
    product_version="${VOGT_PRODUCT_VERSION#v}"; \
    export VOGT_SOURCE_REF="$VOGT_SOURCE_REF" \
        VOGT_SOURCE_SHA="$VOGT_SOURCE_SHA" \
        VOGT_PRODUCT_VERSION="$product_version"; \
    pnpm build

FROM ${RUNTIME_IMAGE} AS runtime
LABEL org.opencontainers.image.title="vogt-web" \
      org.opencontainers.image.description="Vogt PWA bundle for a core-only preview; serves nothing itself" \
      org.opencontainers.image.source="https://github.com/TheDancingDeveloper-org/vogt" \
      org.opencontainers.image.licenses="AGPL-3.0-only"
COPY --from=web-build /app/web/dist /srv/web
# The base's `node` user is uid 1000.
USER 1000:0
CMD ["ls", "/srv/web"]
