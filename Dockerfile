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

# Most tool versions are resolved to "latest at build time" rather than
# pinned — each RUN block below queries the tool's own release API/index and
# verifies whatever it gets against that same source's published checksum
# (still integrity-checked, just not reproducible byte-for-byte build to
# build). Two categories stay pinned deliberately:
#   - NODE_IMAGE/RUST_IMAGE: major-version base image tags. These already
#     float at the patch level; the major version is a stability choice, not
#     staleness.
#   - Android SDK cmdline-tools/platform-tools: Google doesn't publish a
#     "latest" download URL for these (would need parsing their repository
#     XML), so they stay pinned. Bumped to current as of the last audit.
ARG NODE_IMAGE=node:22-bookworm
ARG RUST_IMAGE=rust:1-bookworm
ARG ANDROID_CMDLINE_TOOLS_VERSION=15859902
ARG ANDROID_CMDLINE_TOOLS_SHA256=4e4c464f145a7512b57d088ac6c278c03c9eea610886b35a5e0804e74eedf583
ARG ANDROID_PLATFORM_TOOLS_VERSION=37.0.1
ARG ANDROID_PLATFORM_TOOLS_SHA256=d230f13842f60f782a8645f9c813f8f845bf36089ea7289f28c48f17979313f1
# Off by default (prod). The dev image build passes --build-arg
# INSTALL_AI_CLIENTS=true (.woodpecker/server.yml build-and-push-dev) to
# bake in codex + claude for pre-prod trial — see AGENTS.md "Codex and
# Claude are deliberately not installed by container bootstrap" for why
# prod stays opt-in/user-managed for these two specifically. Intentionally
# unpinned (always latest at build time) per user direction — unlike every
# other tool in this file, these two are expected to move fast and dev is
# where that churn should be absorbed first.
ARG INSTALL_AI_CLIENTS=false

# ─── Stage 1: web bundle ────────────────────────────────────────────────────
FROM ${NODE_IMAGE} AS web-build
WORKDIR /app/web
COPY web/package.json web/pnpm-lock.yaml ./
RUN npm install -g pnpm && pnpm install --frozen-lockfile
COPY web/ ./
RUN pnpm build

# ─── Stage 2: rust binary with embedded web/ ────────────────────────────────
FROM ${RUST_IMAGE} AS server-build
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY contract/Cargo.toml ./contract/Cargo.toml
COPY contract/src ./contract/src
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
# Android SDK lives in /opt (NOT ~/Android/Sdk): the runtime bind-mounts the
# host home over /home/sprooty, so anything under $HOME vanishes at runtime —
# same reason gradle/uv/pnpm are installed system-wide below.
ENV DEBIAN_FRONTEND=noninteractive \
    LANG=C.UTF-8 \
    NPM_CONFIG_PREFIX=/home/sprooty/.npm-global \
    JAVA_HOME=/usr/lib/jvm/java-21-openjdk-amd64 \
    ANDROID_HOME=/opt/android-sdk \
    ANDROID_SDK_ROOT=/opt/android-sdk \
    RUSTUP_HOME=/opt/rust/rustup \
    CARGO_HOME=/opt/rust/cargo \
    PATH=/home/sprooty/.npm-global/bin:/home/sprooty/.local/bin:/opt/rust/cargo/bin:/opt/android-sdk/cmdline-tools/latest/bin:/opt/android-sdk/platform-tools:/opt/android-sdk/build-tools/36.0.0:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin

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
ARG NODE_MAJOR=22
RUN install -m 0755 -d /usr/share/keyrings \
    && curl -fsSL https://deb.nodesource.com/gpgkey/nodesource-repo.gpg.key \
       | gpg --dearmor -o /usr/share/keyrings/nodesource.gpg \
    && chmod 0644 /usr/share/keyrings/nodesource.gpg \
    && arch="$(dpkg --print-architecture)" \
    && printf 'Types: deb\nURIs: https://deb.nodesource.com/node_%s.x\nSuites: nodistro\nComponents: main\nArchitectures: %s\nSigned-By: /usr/share/keyrings/nodesource.gpg\n' \
       "${NODE_MAJOR}" "${arch}" > /etc/apt/sources.list.d/nodesource.sources \
    && printf 'Package: nodejs\nPin: origin deb.nodesource.com\nPin-Priority: 600\n' \
       > /etc/apt/preferences.d/nodejs \
    && printf 'Package: nsolid\nPin: origin deb.nodesource.com\nPin-Priority: 600\n' \
       > /etc/apt/preferences.d/nsolid \
    && apt-get update \
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
        docker-ce-cli \
        docker-compose-plugin \
    && rm -rf /var/lib/apt/lists/*

# GitHub CLI
RUN curl -fsSL https://cli.github.com/packages/githubcli-archive-keyring.gpg \
       | gpg --dearmor -o /usr/share/keyrings/githubcli-archive-keyring.gpg \
    && echo "deb [arch=$(dpkg --print-architecture) signed-by=/usr/share/keyrings/githubcli-archive-keyring.gpg] \
            https://cli.github.com/packages stable main" \
       > /etc/apt/sources.list.d/github-cli.list \
    && apt-get update && apt-get install -y --no-install-recommends gh \
    && rm -rf /var/lib/apt/lists/*

# GitHub MCP server — agent-facing MCP access to the GitHub API (issues, PRs,
# repos, actions, ...). No apt repo; resolve the latest release via the API
# and verify the Linux amd64 tarball against the sha256 digest that same API
# response publishes, same pattern as the step CLI and sccache installs above.
RUN api="https://api.github.com/repos/github/github-mcp-server/releases/latest" \
    && curl -fsSL "$api" -o /tmp/github-mcp-server-release.json \
    && asset_url=$(jq -r '.assets[] | select(.name == "github-mcp-server_Linux_x86_64.tar.gz") | .browser_download_url' /tmp/github-mcp-server-release.json) \
    && asset_digest=$(jq -r '.assets[] | select(.name == "github-mcp-server_Linux_x86_64.tar.gz") | .digest | ltrimstr("sha256:")' /tmp/github-mcp-server-release.json) \
    && curl -fsSL "$asset_url" -o /tmp/github-mcp-server.tar.gz \
    && echo "${asset_digest}  /tmp/github-mcp-server.tar.gz" | sha256sum -c - \
    && tar -xzf /tmp/github-mcp-server.tar.gz -C /tmp \
    && install -m 755 /tmp/github-mcp-server /usr/local/bin/github-mcp-server \
    && rm -f /tmp/github-mcp-server-release.json /tmp/github-mcp-server.tar.gz /tmp/github-mcp-server

# Tailscale (TUN device must be passed in at runtime)
RUN . /etc/os-release \
    && curl -fsSL "https://pkgs.tailscale.com/stable/ubuntu/${VERSION_CODENAME}.noarmor.gpg" \
       > /usr/share/keyrings/tailscale-archive-keyring.gpg \
    && echo "deb [signed-by=/usr/share/keyrings/tailscale-archive-keyring.gpg] \
            https://pkgs.tailscale.com/stable/ubuntu ${VERSION_CODENAME} main" \
       > /etc/apt/sources.list.d/tailscale.list \
    && apt-get update && apt-get install -y --no-install-recommends tailscale \
    && rm -rf /var/lib/apt/lists/*

# rclone — official installer resolves + verifies latest itself.
RUN curl -fsSL https://rclone.org/install.sh | bash

# Infisical CLI
RUN install -m 0755 -d /usr/share/keyrings \
    && curl -1sLf https://artifacts-cli.infisical.com/infisical.gpg \
       | gpg --dearmor -o /usr/share/keyrings/infisical-archive-keyring.gpg \
    && chmod 0644 /usr/share/keyrings/infisical-archive-keyring.gpg \
    && echo "deb [arch=$(dpkg --print-architecture) signed-by=/usr/share/keyrings/infisical-archive-keyring.gpg] https://artifacts-cli.infisical.com/deb stable main" \
       > /etc/apt/sources.list.d/infisical.list \
    && apt-get update \
    && apt-get install -y --no-install-recommends infisical \
    && rm -rf /var/lib/apt/lists/*

# Smallstep `step` CLI — used to self-issue short-lived SSH certificates against
# the step-ca on Node B (`step ssh certificate ...`), the only path to host-shell
# SSH for in-pod agents. Installed from the official .deb (lands at /usr/bin/step).
# No apt repo for this one, and GitHub doesn't offer a stable "latest" download
# URL — resolve the latest release tag via the API, then verify the asset
# against the sha256 digest that same API response publishes.
RUN api="https://api.github.com/repos/smallstep/cli/releases/latest" \
    && curl -fsSL "$api" -o /tmp/step-release.json \
    && step_version=$(jq -r '.tag_name | ltrimstr("v")' /tmp/step-release.json) \
    && asset_url=$(jq -r --arg v "$step_version" '.assets[] | select(.name == "step-cli_" + $v + "-1_amd64.deb") | .browser_download_url' /tmp/step-release.json) \
    && asset_digest=$(jq -r --arg v "$step_version" '.assets[] | select(.name == "step-cli_" + $v + "-1_amd64.deb") | .digest | ltrimstr("sha256:")' /tmp/step-release.json) \
    && curl -fsSL "$asset_url" -o /tmp/step-cli.deb \
    && echo "${asset_digest}  /tmp/step-cli.deb" | sha256sum -c - \
    && apt-get install -y --no-install-recommends /tmp/step-cli.deb \
    && rm -f /tmp/step-cli.deb /tmp/step-release.json \
    && rm -rf /var/lib/apt/lists/*

# Sway (headless Wayland compositor) + minimal apps for in-pod GUI testing
RUN apt-get update && apt-get install -y --no-install-recommends \
        sway swaybg foot wofi grim slurp wl-clipboard xdg-desktop-portal-wlr \
        xwayland chromium \
    && rm -rf /var/lib/apt/lists/*

# GStreamer + WebRTC bits for in-pod GUI streaming.
#
# Selkies-GStreamer was previously installed best-effort because the PyPI name
# was unstable. Installs latest and records the resolved version (via `pip3
# show`) in /etc/mydevenv2/features.json so the server can expose accurate
# "what's available" state via /api/config without misleading users about GUI
# support.
RUN apt-get update && apt-get install -y --no-install-recommends \
        gstreamer1.0-plugins-base gstreamer1.0-plugins-good \
        gstreamer1.0-plugins-bad gstreamer1.0-plugins-ugly \
        gstreamer1.0-libav gstreamer1.0-tools \
        python3-pip libgstreamer1.0-0 \
    && rm -rf /var/lib/apt/lists/*

RUN install -d /etc/mydevenv2 \
    && if pip3 install --break-system-packages --no-cache-dir selkies; then \
        resolved="$(pip3 show selkies 2>/dev/null | sed -n 's/^Version: //p')"; \
        echo "{\"selkies\":\"${resolved}\"}" > /etc/mydevenv2/features.json; \
    else \
        echo "selkies unavailable — GUI streaming disabled in this image" >&2; \
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

# Rust lives in /opt/rust, NOT ~/.rustup + ~/.cargo — same rule as the Android
# SDK above: the runtime bind-mounts the host home over /home/sprooty, so a
# toolchain installed under $HOME is invisible at runtime. It was installed
# under $HOME until 2026-07-31, which silently cost the pod every cargo-installed
# tool (cargo-deb, cargo-zigbuild, cargo-xwin, cargo-watch, rust-analyzer-mcp,
# sccache) and every cross-compile target; only whatever rustup the home volume
# happened to hold was actually reachable.
#
# Consequence to keep in mind: the crate registry cache now lives at
# /opt/rust/cargo/registry inside the image rather than on the persisted home
# volume, so crates re-download after an image redeploy. sccache -> Redis on
# Node B still covers recompilation.
RUN mkdir -p /opt/rust && chown -R sprooty:sprooty /opt/rust

USER sprooty
WORKDIR /home/sprooty
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
       | sh -s -- -y --default-toolchain stable --profile minimal \
           --no-modify-path \
           --component rustfmt --component clippy \
    && rustup component add rust-analyzer \
    && rustup target add x86_64-unknown-linux-musl aarch64-unknown-linux-gnu x86_64-pc-windows-gnu \
    && cargo install --locked cargo-deb \
    && cargo install --locked cargo-zigbuild \
    && cargo install --locked cargo-xwin \
    && cargo install --locked cargo-watch \
    && cargo install --locked rust-analyzer-mcp \
    && test -x /opt/rust/cargo/bin/cargo-zigbuild \
    && test -x /opt/rust/cargo/bin/rust-analyzer-mcp

# opencode's installer hardcodes $HOME/.opencode/bin with no override, so
# relocate the binary to /usr/local/bin afterwards — otherwise the runtime
# /home/sprooty bind mount hides it, same as the Rust tools above.
RUN curl -fsSL https://opencode.ai/install | bash -s -- --no-modify-path \
    && sudo install -m 755 /home/sprooty/.opencode/bin/opencode /usr/local/bin/opencode \
    && rm -rf /home/sprooty/.opencode

ARG INSTALL_AI_CLIENTS
# --prefix=/usr/local (not the ambient $NPM_CONFIG_PREFIX, which points at
# /home/sprooty/.npm-global) so these survive the runtime /home/sprooty bind
# mount — same reasoning as the system pnpm install above. sudo because
# /usr/local isn't sprooty-writable.
RUN if [ "$INSTALL_AI_CLIENTS" = "true" ]; then \
        sudo npm install -g --prefix=/usr/local @openai/codex @anthropic-ai/claude-code ; \
    fi

# sccache (apt package lacks Redis support; pull from GitHub). Resolve latest
# release via the API and verify against the sha256 digest that same
# response publishes for the asset — no hardcoded checksum to keep in sync.
RUN api="https://api.github.com/repos/mozilla/sccache/releases/latest" \
    && curl -fsSL "$api" -o /tmp/sccache-release.json \
    && sccache_version=$(jq -r '.tag_name | ltrimstr("v")' /tmp/sccache-release.json) \
    && asset_name="sccache-v${sccache_version}-x86_64-unknown-linux-musl.tar.gz" \
    && asset_url=$(jq -r --arg n "$asset_name" '.assets[] | select(.name == $n) | .browser_download_url' /tmp/sccache-release.json) \
    && asset_digest=$(jq -r --arg n "$asset_name" '.assets[] | select(.name == $n) | .digest | ltrimstr("sha256:")' /tmp/sccache-release.json) \
    && curl -fsSL "$asset_url" -o /tmp/sccache.tar.gz \
    && echo "${asset_digest}  /tmp/sccache.tar.gz" | sha256sum -c - \
    && tar -xzf /tmp/sccache.tar.gz -C /tmp \
    && mv "/tmp/sccache-v${sccache_version}-x86_64-unknown-linux-musl/sccache" /opt/rust/cargo/bin/sccache \
    && chmod +x /opt/rust/cargo/bin/sccache \
    && rm -f /tmp/sccache-release.json /tmp/sccache.tar.gz

USER root

# Java + Gradle. Installed globally (/opt + a /usr/local/bin symlink) so they
# survive the /home/sprooty bind mount at runtime. gradle finds the JDK via
# `java` on PATH, so no JAVA_HOME juggling needed. This system gradle is only
# a convenience for ad-hoc use — the actual mobile build is driven by the
# committed wrapper (mobile/android/gradlew), which pins its own version
# independently, so unpinning this one doesn't affect build reproducibility.
# services.gradle.org publishes a "current" endpoint with the download URL
# and checksum together, so no separate lookup needed.
RUN apt-get update && apt-get install -y --no-install-recommends \
        openjdk-21-jdk-headless \
    && rm -rf /var/lib/apt/lists/* \
    && curl -fsSL https://services.gradle.org/versions/current -o /tmp/gradle-current.json \
    && gradle_version=$(jq -r '.version' /tmp/gradle-current.json) \
    && gradle_checksum=$(jq -r '.checksum' /tmp/gradle-current.json) \
    && curl -fsSL "https://services.gradle.org/distributions/gradle-${gradle_version}-bin.zip" \
       -o /tmp/gradle.zip \
    && echo "${gradle_checksum}  /tmp/gradle.zip" | sha256sum -c - \
    && unzip -q /tmp/gradle.zip -d /opt \
    && ln -s "/opt/gradle-${gradle_version}/bin/gradle" /usr/local/bin/gradle \
    && rm /tmp/gradle.zip /tmp/gradle-current.json

# Android SDK (for the Capacitor android project under mobile/). Installed to
# /opt/android-sdk so it survives the /home/sprooty bind mount at runtime — a
# ~/Android/Sdk install would be shadowed. sdkmanager/adb/apkanalyzer are put
# on PATH via the ANDROID_HOME entries in the stage-3 ENV above. The committed
# Gradle wrapper (mobile/android/gradlew) drives builds; system gradle is only
# a convenience for ad-hoc use.
#
# Package set per the dev-pod spec: pinned command-line tools + platform-tools,
# platforms;android-35 + android-36, build-tools;35.0.0 + 36.0.0.
ARG ANDROID_CMDLINE_TOOLS_VERSION
ARG ANDROID_CMDLINE_TOOLS_SHA256
ARG ANDROID_PLATFORM_TOOLS_VERSION
ARG ANDROID_PLATFORM_TOOLS_SHA256
RUN install -d /opt/android-sdk/cmdline-tools \
    && curl -fsSL \
        "https://dl.google.com/android/repository/commandlinetools-linux-${ANDROID_CMDLINE_TOOLS_VERSION}_latest.zip" \
        -o /tmp/cmdline-tools.zip \
    && echo "${ANDROID_CMDLINE_TOOLS_SHA256}  /tmp/cmdline-tools.zip" | sha256sum -c - \
    && unzip -q /tmp/cmdline-tools.zip -d /tmp/cmdline-tools \
    # The zip unpacks to a top-level cmdline-tools/; sdkmanager expects it at
    # cmdline-tools/latest/.
    && mv /tmp/cmdline-tools/cmdline-tools /opt/android-sdk/cmdline-tools/latest \
    && curl -fsSL \
        "https://dl.google.com/android/repository/platform-tools_r${ANDROID_PLATFORM_TOOLS_VERSION}-linux.zip" \
        -o /tmp/platform-tools.zip \
    && echo "${ANDROID_PLATFORM_TOOLS_SHA256}  /tmp/platform-tools.zip" | sha256sum -c - \
    && unzip -q /tmp/platform-tools.zip -d /opt/android-sdk \
    && rm -rf /tmp/cmdline-tools.zip /tmp/cmdline-tools \
        /tmp/platform-tools.zip \
    # Accept licenses first (yes feeds the interactive prompts), then install.
    && yes | /opt/android-sdk/cmdline-tools/latest/bin/sdkmanager --licenses >/dev/null \
    && /opt/android-sdk/cmdline-tools/latest/bin/sdkmanager --install \
        "platforms;android-35" \
        "platforms;android-36" \
        "build-tools;35.0.0" \
        "build-tools;36.0.0" \
    # Owned by sprooty so in-pod Gradle builds can write license acks and any
    # auto-managed SDK components at runtime (the SDK lives in /opt, which is
    # not bind-mounted, so this ownership persists from the image).
    && chown -R ${SPROOTY_UID}:${SPROOTY_GID} /opt/android-sdk

# Python tools the user expects (uv, ruff, pytest). Installed globally into
# /usr/local (system pip lands scripts in /usr/local/bin) so they survive the
# /home/sprooty bind mount — a --user install would be shadowed at runtime.
# Fail the build if they don't install — silent fallback would leave the pod
# with broken Python tooling that surfaces as cryptic command-not-found errors.
RUN pip3 install --break-system-packages --no-cache-dir uv ruff pytest

# The server binary (built in stage 2 with the web bundle embedded; cache
# mounts in stage 2 mean we have to copy out of /usr/local/bin, not /app).
COPY --from=server-build /usr/local/bin/mydevenv2-server /usr/local/bin/mydevenv2-server
RUN chmod +x /usr/local/bin/mydevenv2-server

# Runtime scripts. Agent-specific CLIs are optional; these helpers only broker
# credentials on demand for neutral tools already present in the image.
COPY deploy/entrypoint.sh /usr/local/bin/mydevenv2-entrypoint
COPY deploy/agent-auth.sh /usr/local/bin/mydevenv2-agent-auth
COPY deploy/git-askpass.sh /usr/local/bin/mydevenv2-git-askpass
COPY deploy/rust-analyzer-mcp.sh /usr/local/bin/mydevenv2-rust-analyzer-mcp
RUN chmod +x \
    /usr/local/bin/mydevenv2-entrypoint \
    /usr/local/bin/mydevenv2-agent-auth \
    /usr/local/bin/mydevenv2-git-askpass \
    /usr/local/bin/mydevenv2-rust-analyzer-mcp

EXPOSE 8910
VOLUME ["/home/sprooty/Working"]

USER sprooty
WORKDIR /home/sprooty/Working

ENV MYDEVENV2_BIND=0.0.0.0:8910
ENV RUST_LOG=info

ENTRYPOINT ["/usr/local/bin/mydevenv2-entrypoint"]
CMD []
