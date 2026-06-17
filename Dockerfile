# MyDevEnv2 — single-image dev pod.
#
# Three stages:
#   1. web-build   — pnpm install + vite build of web/
#   2. server-build — cargo build --release of server/, embedding the bundle
#   3. runtime     — Ubuntu 26.04 with the full TOOLING.md toolchain, sway,
#                    selkies-gstreamer, and the binary copied in
#
# Built and pushed by .woodpecker.yml as repo.indexarr.net/indexarr/mydevenv2.
# Deployed via Komodo (see deploy/docker-compose.yml).

# Pinned tool versions. Bump deliberately, alongside a fresh rebuild + smoke
# test on the target periphery. Out-of-band pins also live in the woodpecker
# pipeline so CI builds bit-identical artefacts.
ARG NODE_IMAGE=node:22-bookworm
ARG RUST_IMAGE=rust:1.95-bookworm
ARG PNPM_VERSION=10.18.0
ARG SCCACHE_VERSION=0.10.0
ARG SELKIES_VERSION=1.6.2

# ─── Stage 1: web bundle ────────────────────────────────────────────────────
FROM ${NODE_IMAGE} AS web-build
WORKDIR /app/web
ARG PNPM_VERSION
COPY web/package.json web/pnpm-lock.yaml ./
RUN npm install -g pnpm@${PNPM_VERSION} && pnpm install --frozen-lockfile
COPY web/ ./
RUN pnpm build

# ─── Stage 2: rust binary with embedded web/ ────────────────────────────────
FROM ${RUST_IMAGE} AS server-build
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY server/Cargo.toml ./server/Cargo.toml
COPY server/src ./server/src
COPY --from=web-build /app/web/dist ./web/dist
# BuildKit cache mounts keep registry/git deps + the target dir warm across
# rebuilds without the brittle "dummy main.rs to prime deps" dance.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/app/target \
    cargo build --release -p mydevenv2-server \
    && cp target/release/mydevenv2-server /usr/local/bin/mydevenv2-server \
    && strip /usr/local/bin/mydevenv2-server

# ─── Stage 3: runtime + dev tooling ─────────────────────────────────────────
FROM ubuntu:26.04
ENV DEBIAN_FRONTEND=noninteractive \
    LANG=C.UTF-8 \
    NPM_CONFIG_PREFIX=/home/sprooty/.npm-global \
    PATH=/home/sprooty/.npm-global/bin:/home/sprooty/.local/bin:/home/sprooty/.cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin

# Core system + dev utilities (per TOOLING.md)
RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates curl wget gnupg lsb-release \
        git git-lfs vim nano less man-db sudo \
        build-essential pkg-config cmake clang lld nasm \
        jq ripgrep fd-find bat rsync \
        openssh-client openssh-server iputils-ping netcat-openbsd dnsutils xdg-utils \
        htop tree file unzip zip \
        musl-tools gcc-mingw-w64-x86-64 gcc-aarch64-linux-gnu g++-aarch64-linux-gnu \
        libssl-dev libclang-dev protobuf-compiler \
        python3 python3-pip python3-venv python3-dev \
    && rm -rf /var/lib/apt/lists/*

# Node 22 + pnpm. Override NPM_CONFIG_PREFIX for this one install so pnpm
# lands in /usr/local (survives the runtime /home/sprooty bind mount).
# User-installed globals go to $NPM_CONFIG_PREFIX (/home/sprooty/.npm-global).
RUN curl -fsSL https://deb.nodesource.com/setup_22.x | bash - \
    && apt-get install -y --no-install-recommends nodejs \
    && npm install -g --prefix=/usr/local pnpm \
    && rm -rf /var/lib/apt/lists/*

# Docker CLI (DooD pattern — docker.sock mounted from host)
RUN . /etc/os-release \
    && install -m 0755 -d /etc/apt/keyrings \
    && curl -fsSL https://download.docker.com/linux/ubuntu/gpg \
       | gpg --dearmor -o /etc/apt/keyrings/docker.gpg \
    && chmod a+r /etc/apt/keyrings/docker.gpg \
    && echo "deb [arch=$(dpkg --print-architecture) signed-by=/etc/apt/keyrings/docker.gpg] \
            https://download.docker.com/linux/ubuntu $VERSION_CODENAME stable" \
       > /etc/apt/sources.list.d/docker.list \
    && apt-get update \
    && apt-get install -y --no-install-recommends \
        docker-ce-cli docker-compose-plugin \
    && rm -rf /var/lib/apt/lists/*

# GitHub CLI
RUN curl -fsSL https://cli.github.com/packages/githubcli-archive-keyring.gpg \
       | gpg --dearmor -o /usr/share/keyrings/githubcli-archive-keyring.gpg \
    && echo "deb [arch=$(dpkg --print-architecture) signed-by=/usr/share/keyrings/githubcli-archive-keyring.gpg] \
            https://cli.github.com/packages stable main" \
       > /etc/apt/sources.list.d/github-cli.list \
    && apt-get update && apt-get install -y --no-install-recommends gh \
    && rm -rf /var/lib/apt/lists/*

# Tailscale (TUN device must be passed in at runtime)
RUN . /etc/os-release \
    && curl -fsSL "https://pkgs.tailscale.com/stable/ubuntu/${VERSION_CODENAME}.noarmor.gpg" \
       > /usr/share/keyrings/tailscale-archive-keyring.gpg \
    && echo "deb [signed-by=/usr/share/keyrings/tailscale-archive-keyring.gpg] \
            https://pkgs.tailscale.com/stable/ubuntu ${VERSION_CODENAME} main" \
       > /etc/apt/sources.list.d/tailscale.list \
    && apt-get update && apt-get install -y --no-install-recommends tailscale \
    && rm -rf /var/lib/apt/lists/*

# rclone
RUN curl -fsSL https://rclone.org/install.sh | bash

# Infisical CLI
RUN curl -1sLf 'https://artifacts-cli.infisical.com/setup.deb.sh' | bash \
    && apt-get install -y --no-install-recommends infisical \
    && rm -rf /var/lib/apt/lists/*

# Sway (headless Wayland compositor) + minimal apps for in-pod GUI testing
RUN apt-get update && apt-get install -y --no-install-recommends \
        sway swaybg foot wofi grim slurp wl-clipboard xdg-desktop-portal-wlr \
        xwayland chromium \
    && rm -rf /var/lib/apt/lists/*

# GStreamer + WebRTC bits for in-pod GUI streaming.
#
# Selkies-GStreamer was previously installed best-effort because the PyPI name
# was unstable. We now pin a known-good version and record the result in
# /etc/mydevenv2/features.json so the server can expose accurate "what's
# available" state via /api/config without misleading users about GUI support.
RUN apt-get update && apt-get install -y --no-install-recommends \
        gstreamer1.0-plugins-base gstreamer1.0-plugins-good \
        gstreamer1.0-plugins-bad gstreamer1.0-plugins-ugly \
        gstreamer1.0-libav gstreamer1.0-tools \
        python3-pip libgstreamer1.0-0 \
    && rm -rf /var/lib/apt/lists/*

ARG SELKIES_VERSION
RUN install -d /etc/mydevenv2 \
    && if pip3 install --break-system-packages --no-cache-dir "selkies==${SELKIES_VERSION}"; then \
        echo "{\"selkies\":\"${SELKIES_VERSION}\"}" > /etc/mydevenv2/features.json; \
    else \
        echo "selkies==${SELKIES_VERSION} unavailable — GUI streaming disabled in this image" >&2; \
        echo "{\"selkies\":null}" > /etc/mydevenv2/features.json; \
    fi

# Rust toolchain (full dev env so user can build inside the pod too)
ARG SPROOTY_UID=1000
ARG SPROOTY_GID=1000
# Ubuntu 26.04 ships a default `ubuntu` user at UID/GID 1000 — remove it so
# we own those numbers (matters because the host bind-mounts /home/sprooty
# expect that uid).
RUN userdel -r ubuntu 2>/dev/null || true \
    && groupdel ubuntu 2>/dev/null || true \
    && groupadd -g ${SPROOTY_GID} sprooty \
    && useradd -m -s /bin/bash -u ${SPROOTY_UID} -g ${SPROOTY_GID} sprooty \
    && usermod -aG sudo sprooty \
    && echo 'sprooty ALL=(ALL) NOPASSWD:ALL' > /etc/sudoers.d/90-sprooty

USER sprooty
WORKDIR /home/sprooty
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
       | sh -s -- -y --default-toolchain stable --profile minimal \
           --component rustfmt --component clippy \
    && rustup target add x86_64-unknown-linux-musl aarch64-unknown-linux-gnu x86_64-pc-windows-gnu \
    && cargo install cargo-deb cargo-zigbuild cargo-xwin cargo-watch

# sccache (apt package lacks Redis support; pull from GitHub)
ARG SCCACHE_VERSION
RUN curl -fsSL "https://github.com/mozilla/sccache/releases/download/v${SCCACHE_VERSION}/sccache-v${SCCACHE_VERSION}-x86_64-unknown-linux-musl.tar.gz" \
       | tar -xz -C /tmp \
    && mv "/tmp/sccache-v${SCCACHE_VERSION}-x86_64-unknown-linux-musl/sccache" /home/sprooty/.cargo/bin/sccache \
    && chmod +x /home/sprooty/.cargo/bin/sccache

USER root

# Java + Gradle. Installed globally (/opt + a /usr/local/bin symlink) so they
# survive the /home/sprooty bind mount at runtime. gradle finds the JDK via
# `java` on PATH, so no JAVA_HOME juggling needed.
ARG GRADLE_VERSION=8.12
RUN apt-get update && apt-get install -y --no-install-recommends \
        openjdk-21-jdk-headless \
    && rm -rf /var/lib/apt/lists/* \
    && curl -fsSL "https://services.gradle.org/distributions/gradle-${GRADLE_VERSION}-bin.zip" \
       -o /tmp/gradle.zip \
    && unzip -q /tmp/gradle.zip -d /opt \
    && ln -s "/opt/gradle-${GRADLE_VERSION}/bin/gradle" /usr/local/bin/gradle \
    && rm /tmp/gradle.zip

# Python tools the user expects (uv, ruff, pytest). Installed globally into
# /usr/local (system pip lands scripts in /usr/local/bin) so they survive the
# /home/sprooty bind mount — a --user install would be shadowed at runtime.
# Fail the build if they don't install — silent fallback would leave the pod
# with broken Python tooling that surfaces as cryptic command-not-found errors.
ARG UV_VERSION=0.5.20
ARG RUFF_VERSION=0.7.4
ARG PYTEST_VERSION=8.3.4
RUN pip3 install --break-system-packages --no-cache-dir \
        "uv==${UV_VERSION}" "ruff==${RUFF_VERSION}" "pytest==${PYTEST_VERSION}"

# The server binary (built in stage 2 with the web bundle embedded; cache
# mounts in stage 2 mean we have to copy out of /usr/local/bin, not /app).
COPY --from=server-build /usr/local/bin/mydevenv2-server /usr/local/bin/mydevenv2-server
RUN chmod +x /usr/local/bin/mydevenv2-server

# Runtime scripts. Agent-specific CLIs are optional; these helpers only broker
# credentials on demand for neutral tools already present in the image.
COPY deploy/entrypoint.sh /usr/local/bin/mydevenv2-entrypoint
COPY deploy/agent-auth.sh /usr/local/bin/mydevenv2-agent-auth
COPY deploy/git-askpass.sh /usr/local/bin/mydevenv2-git-askpass
RUN chmod +x \
    /usr/local/bin/mydevenv2-entrypoint \
    /usr/local/bin/mydevenv2-agent-auth \
    /usr/local/bin/mydevenv2-git-askpass

EXPOSE 8910
VOLUME ["/home/sprooty/Working"]

USER sprooty
WORKDIR /home/sprooty/Working

ENV MYDEVENV2_BIND=0.0.0.0:8910
ENV RUST_LOG=info

ENTRYPOINT ["/usr/local/bin/mydevenv2-entrypoint"]
CMD []
