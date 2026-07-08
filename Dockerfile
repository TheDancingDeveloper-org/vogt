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
ARG SELKIES_VERSION=1.6.1
ARG RUST_TOOLCHAIN=1.95.0
ARG NODEJS_APT_VERSION=22.23.1-1nodesource1
ARG DOCKER_CE_CLI_APT_VERSION=5:29.6.1-1~ubuntu.26.04~resolute
ARG DOCKER_COMPOSE_PLUGIN_APT_VERSION=5.3.1-1~ubuntu.26.04~resolute
ARG GH_APT_VERSION=2.96.0
ARG TAILSCALE_APT_VERSION=1.98.8
ARG INFISICAL_APT_VERSION=0.43.101
ARG CARGO_DEB_VERSION=3.7.0
ARG CARGO_ZIGBUILD_VERSION=0.23.0
ARG CARGO_XWIN_VERSION=0.23.0
ARG CARGO_WATCH_VERSION=8.5.3
ARG SCCACHE_SHA256=1fbb35e135660d04a2d5e42b59c7874d39b3deb17de56330b25b713ec59f849b
ARG STEP_CLI_SHA256=5845c181251ffe43ca2331bc171e0b92324a71be9cf4ef76cd6fbbba4f2a3cc6
ARG GRADLE_SHA256=7a00d51fb93147819aab76024feece20b6b84e420694101f276be952e08bef03
ARG ANDROID_CMDLINE_TOOLS_VERSION=15641748
ARG ANDROID_CMDLINE_TOOLS_SHA256=a66d5ef0238fc0162e9c1446602ce0dd41702d4dd7a94d2ce42d12b7f80baf7e
ARG ANDROID_PLATFORM_TOOLS_VERSION=37.0.0
ARG ANDROID_PLATFORM_TOOLS_SHA256=198ae156ab285fa555987219af237b31102fefe8b9d2bc274708a8d4f2865a07
ARG RCLONE_VERSION=1.74.3
ARG RCLONE_SHA256=dbee7ccd7a5d617e4ed4cd4555c16669b511abfe8d31164f61be35ac9e999bd2

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
    PATH=/home/sprooty/.npm-global/bin:/home/sprooty/.local/bin:/home/sprooty/.cargo/bin:/opt/android-sdk/cmdline-tools/latest/bin:/opt/android-sdk/platform-tools:/opt/android-sdk/build-tools/36.0.0:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin

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
ARG PNPM_VERSION
ARG NODE_MAJOR=22
ARG NODEJS_APT_VERSION
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
    && apt-get install -y --no-install-recommends "nodejs=${NODEJS_APT_VERSION}" \
    && npm install -g --prefix=/usr/local "pnpm@${PNPM_VERSION}" \
    && rm -rf /var/lib/apt/lists/*

# Docker CLI (DooD pattern — docker.sock mounted from host)
ARG DOCKER_CE_CLI_APT_VERSION
ARG DOCKER_COMPOSE_PLUGIN_APT_VERSION
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
        "docker-ce-cli=${DOCKER_CE_CLI_APT_VERSION}" \
        "docker-compose-plugin=${DOCKER_COMPOSE_PLUGIN_APT_VERSION}" \
    && rm -rf /var/lib/apt/lists/*

# GitHub CLI
ARG GH_APT_VERSION
RUN curl -fsSL https://cli.github.com/packages/githubcli-archive-keyring.gpg \
       | gpg --dearmor -o /usr/share/keyrings/githubcli-archive-keyring.gpg \
    && echo "deb [arch=$(dpkg --print-architecture) signed-by=/usr/share/keyrings/githubcli-archive-keyring.gpg] \
            https://cli.github.com/packages stable main" \
       > /etc/apt/sources.list.d/github-cli.list \
    && apt-get update && apt-get install -y --no-install-recommends "gh=${GH_APT_VERSION}" \
    && rm -rf /var/lib/apt/lists/*

# Tailscale (TUN device must be passed in at runtime)
ARG TAILSCALE_APT_VERSION
RUN . /etc/os-release \
    && curl -fsSL "https://pkgs.tailscale.com/stable/ubuntu/${VERSION_CODENAME}.noarmor.gpg" \
       > /usr/share/keyrings/tailscale-archive-keyring.gpg \
    && echo "deb [signed-by=/usr/share/keyrings/tailscale-archive-keyring.gpg] \
            https://pkgs.tailscale.com/stable/ubuntu ${VERSION_CODENAME} main" \
       > /etc/apt/sources.list.d/tailscale.list \
    && apt-get update && apt-get install -y --no-install-recommends "tailscale=${TAILSCALE_APT_VERSION}" \
    && rm -rf /var/lib/apt/lists/*

# rclone
ARG RCLONE_VERSION
ARG RCLONE_SHA256
RUN curl -fsSL "https://downloads.rclone.org/v${RCLONE_VERSION}/rclone-v${RCLONE_VERSION}-linux-amd64.zip" \
       -o /tmp/rclone.zip \
    && echo "${RCLONE_SHA256}  /tmp/rclone.zip" | sha256sum -c - \
    && unzip -q /tmp/rclone.zip -d /tmp/rclone \
    && install -m 0755 "/tmp/rclone/rclone-v${RCLONE_VERSION}-linux-amd64/rclone" /usr/local/bin/rclone \
    && rm -rf /tmp/rclone /tmp/rclone.zip

# Infisical CLI
ARG INFISICAL_APT_VERSION
RUN install -m 0755 -d /usr/share/keyrings \
    && curl -1sLf https://artifacts-cli.infisical.com/infisical.gpg \
       | gpg --dearmor -o /usr/share/keyrings/infisical-archive-keyring.gpg \
    && chmod 0644 /usr/share/keyrings/infisical-archive-keyring.gpg \
    && echo "deb [arch=$(dpkg --print-architecture) signed-by=/usr/share/keyrings/infisical-archive-keyring.gpg] https://artifacts-cli.infisical.com/deb stable main" \
       > /etc/apt/sources.list.d/infisical.list \
    && apt-get update \
    && apt-get install -y --no-install-recommends "infisical=${INFISICAL_APT_VERSION}" \
    && rm -rf /var/lib/apt/lists/*

# Smallstep `step` CLI — used to self-issue short-lived SSH certificates against
# the step-ca on Node B (`step ssh certificate ...`), the only path to host-shell
# SSH for in-pod agents. Installed from the official .deb (lands at /usr/bin/step).
ARG STEP_CLI_VERSION=0.30.6
ARG STEP_CLI_SHA256
RUN curl -fsSL \
        "https://github.com/smallstep/cli/releases/download/v${STEP_CLI_VERSION}/step-cli_${STEP_CLI_VERSION}-1_amd64.deb" \
        -o /tmp/step-cli.deb \
    && echo "${STEP_CLI_SHA256}  /tmp/step-cli.deb" | sha256sum -c - \
    && apt-get install -y --no-install-recommends /tmp/step-cli.deb \
    && rm -f /tmp/step-cli.deb \
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
ARG RUST_TOOLCHAIN
ARG CARGO_DEB_VERSION
ARG CARGO_ZIGBUILD_VERSION
ARG CARGO_XWIN_VERSION
ARG CARGO_WATCH_VERSION
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
       | sh -s -- -y --default-toolchain ${RUST_TOOLCHAIN} --profile minimal \
           --component rustfmt --component clippy \
    && rustup target add x86_64-unknown-linux-musl aarch64-unknown-linux-gnu x86_64-pc-windows-gnu \
    && cargo install --locked "cargo-deb@${CARGO_DEB_VERSION}" \
    && cargo install --locked "cargo-zigbuild@${CARGO_ZIGBUILD_VERSION}" \
    && cargo install --locked "cargo-xwin@${CARGO_XWIN_VERSION}" \
    && cargo install --locked "cargo-watch@${CARGO_WATCH_VERSION}"

# sccache (apt package lacks Redis support; pull from GitHub)
ARG SCCACHE_VERSION
ARG SCCACHE_SHA256
RUN curl -fsSL "https://github.com/mozilla/sccache/releases/download/v${SCCACHE_VERSION}/sccache-v${SCCACHE_VERSION}-x86_64-unknown-linux-musl.tar.gz" \
       -o /tmp/sccache.tar.gz \
    && echo "${SCCACHE_SHA256}  /tmp/sccache.tar.gz" | sha256sum -c - \
    && tar -xzf /tmp/sccache.tar.gz -C /tmp \
    && mv "/tmp/sccache-v${SCCACHE_VERSION}-x86_64-unknown-linux-musl/sccache" /home/sprooty/.cargo/bin/sccache \
    && chmod +x /home/sprooty/.cargo/bin/sccache

USER root

# Java + Gradle. Installed globally (/opt + a /usr/local/bin symlink) so they
# survive the /home/sprooty bind mount at runtime. gradle finds the JDK via
# `java` on PATH, so no JAVA_HOME juggling needed.
ARG GRADLE_VERSION=8.12
ARG GRADLE_SHA256
RUN apt-get update && apt-get install -y --no-install-recommends \
        openjdk-21-jdk-headless \
    && rm -rf /var/lib/apt/lists/* \
    && curl -fsSL "https://services.gradle.org/distributions/gradle-${GRADLE_VERSION}-bin.zip" \
       -o /tmp/gradle.zip \
    && echo "${GRADLE_SHA256}  /tmp/gradle.zip" | sha256sum -c - \
    && unzip -q /tmp/gradle.zip -d /opt \
    && ln -s "/opt/gradle-${GRADLE_VERSION}/bin/gradle" /usr/local/bin/gradle \
    && rm /tmp/gradle.zip

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
