# syntax=docker/dockerfile:1
#
# Vogt — the merged image (NFR-D11).
#
# One image, two processes, one published port: the Rust engine is the front
# door and vogt-core runs beside it on loopback. Both halves are built here.
#
# **The build context is the repository root, not `engine/`.** Two things force
# that and neither is negotiable:
#
#   * `server/src/assets.rs` embeds `../../web/dist/`, which from
#     `engine/server/` is `<repo>/web/dist` — the PWA moved to the repository
#     root when the trees were merged, so a build rooted at `engine/` cannot
#     see the bundle it is supposed to embed.
#   * The Python core is `<repo>/pyproject.toml` + `<repo>/src`. An image that
#     ships both halves has to be able to read both halves.
#
# So: `docker build -f engine/Dockerfile .` from the repository root. The
# context is trimmed by `engine/Dockerfile.dockerignore`, which BuildKit reads
# in preference to any `.dockerignore` at the context root — so this build
# stays lean without changing what the root `Dockerfile` (the core-only image,
# still published by .github/workflows) sees.
#
# Stages:
#   1. web-build    — pnpm install + vite build of web/
#   2. server-build — cargo build --release of engine/server, embedding the bundle
#   3. core         — the published public core image, lifted whole (#143)
#   4. runtime      — `FROM ${POD_BASE_IMAGE}` plus both halves + entrypoint
#
# Stage order matters in one place: rust-embed reads `web/dist` at *compile*
# time, so the web bundle is a build input to the Rust binary rather than a
# runtime asset. Stage 2 cannot start before stage 1 finishes.
#
# The dev-pod toolchain — Node, the Rust dev toolchain, sway, selkies,
# Java/Gradle, the Android SDK, Flutter — is no longer installed here (#184). It
# is `engine/Dockerfile.pod`, published as `vogt-pod-base:{lean,full}-*` by
# `.github/workflows/pod-base.yml` and passed in as `POD_BASE_IMAGE`. It was 22
# of this image's 34 build minutes and none of it depends on this repository's
# source; that file's header carries the measurement.
#
# Why the core-only image at the repository root still exists: it is the
# hardened artifact (`read_only`, `cap_drop: [ALL]`, runnable as any uid) that
# Topology A and the standalone core deployments use, and NFR-Q6 keeps
# vogt-core independently runnable on purpose. This image cannot be that: it
# is a development pod — sudo, agent CLIs, a writable home, kernel-mode
# Tailscale — and collapsing the two would quietly delete vogt's containment
# story under an unchanged image name.
#
# Deployed via Komodo as one stack (see ../deploy/vogt-stack.compose.yml).

# The two Docker Hub library bases used by stages 1 and 2 are named through
# this organisation's GHCR mirror rather than through Docker Hub (#33). Builds
# kept failing at `FROM` with 429: a self-hosted runner is one IP for every
# build this repository makes, and authenticating measurably made it worse,
# because the available credential is estate-wide and its bucket is empty
# exactly when the estate is busy. `.github/workflows/mirror-base-images.yml`
# copies each manifest list into GHCR verbatim — the index survives, so these
# still resolve per-platform — and runs as a prerequisite of every build, so a
# Renovate bump is mirrored before anything tries to build from it. The runtime
# base (Ubuntu) moved with the toolchain into `engine/Dockerfile.pod`; the
# mirror workflow reads that file too.
#
# The mirror name is the upstream name with `docker.io/library/` replaced by
# `ghcr.io/thedancingdeveloper-org/vogt-base/`, and that rule is the only
# thing tying the two together: the mirror workflow reads these lines to
# decide what to copy, and `.github/renovate.json5` applies the same
# substitution in reverse so a bump still comes from upstream. Overriding
# these build args back to their Docker Hub names still works and is the
# escape hatch for a builder that cannot reach GHCR.
ARG NODE_IMAGE=ghcr.io/thedancingdeveloper-org/vogt-base/node:22-bookworm
ARG RUST_IMAGE=ghcr.io/thedancingdeveloper-org/vogt-base/rust:1-bookworm
# uv builds the core's virtualenv from the committed lockfile, so the image
# contains exactly what CI tested (NFR-Q5). Same pin as the root Dockerfile;
# they should move together.
ARG UV_IMAGE=ghcr.io/astral-sh/uv:0.9.18
# The dev-pod toolchain base — everything stage 4 used to install itself, and
# 22 of this image's 34 build minutes (#184). Built and published by
# `.github/workflows/pod-base.yml` from `engine/Dockerfile.pod`; see either for
# the variants and the tagging.
#
# CI always overrides this with a **digest**, resolved by that workflow. The
# default here names a moving tag and is a readable placeholder for a local
# build, not a supported way to produce a shipped image — which is why the pin
# lives in the workflow rather than in this line, unlike the mirrored bases
# above whose digests Renovate maintains here.
ARG POD_BASE_IMAGE=ghcr.io/thedancingdeveloper-org/vogt-pod-base:lean
# The published public core image, digest-pinned. No default: see stage 3.
ARG CORE_IMAGE
# Off by default (prod). `.github/workflows/build.yml` passes
# INSTALL_AI_CLIENTS=true when the ref is the `dev` branch — the branch that
# also produces the `:dev` and `dev-<sha>` tags — to bake in codex + claude
# for pre-prod trial. See AGENTS.md "Codex and Claude are deliberately not
# installed by container bootstrap" for why prod stays opt-in/user-managed
# for these two specifically.
#
# This comment used to name `.woodpecker/server.yml build-and-push-dev`. That
# pipeline was retired in the move to Actions and nothing took over the build
# arg, so the default stood and `vogt-dev` shipped with neither client (#23).
# A default is only safe while something is documented to override it — and
# the something has to exist.
ARG INSTALL_AI_CLIENTS=false
# Flutter is dev-only, and is now the thing that separates the two published
# variants of the pod base rather than a build arg here (#184): `dev` is built
# on `vogt-pod-base:full-*`; main and every release on `:lean-*`. See
# `engine/Dockerfile.pod`.
# These are private-stack compatibility features, not parts of the generic
# Vogt delivery. The repository-root image never sees these arguments. Keeping
# them off by default also means a direct engine build does not silently add a
# separate product or provider-specific client to the image.
ARG INSTALL_CADASTRE_MCP=false
ARG INSTALL_THECLAWBAY=false

# ─── Stage 1: web bundle ────────────────────────────────────────────────────
FROM ${NODE_IMAGE} AS web-build
WORKDIR /app/web
COPY web/package.json web/pnpm-lock.yaml ./
RUN npm install -g pnpm && pnpm install --frozen-lockfile
COPY web/ ./
RUN pnpm build

# ─── Stage 2: rust binary with embedded web/ ────────────────────────────────
FROM ${RUST_IMAGE} AS server-build
# The layout inside this stage mirrors the repository's, because rust-embed
# resolves `../../web/dist/` against the *crate* directory. Build the workspace
# at /app/engine and the bundle lands where the macro already looks: put the
# crate at /app/server instead and it would read /web/dist, which is nothing.
WORKDIR /app/engine
COPY engine/Cargo.toml engine/Cargo.lock ./
COPY engine/contract/Cargo.toml ./contract/Cargo.toml
COPY engine/contract/src ./contract/src
COPY engine/server/Cargo.toml ./server/Cargo.toml
COPY engine/server/src ./server/src
COPY --from=web-build /app/web/dist /app/web/dist
# BuildKit cache mounts keep registry/git deps + the target dir warm across
# rebuilds without the brittle "dummy main.rs to prime deps" dance. They are
# builder-local state (not layers), so they warm a repeat build on the same
# worker; the registry layer cache set in `build.yml`/`release.yml` is what
# survives across the node-b fleet for everything that *is* a layer.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/app/engine/target \
    cargo build --release -p mydevenv2-server \
    && cp target/release/mydevenv2-server /usr/local/bin/mydevenv2-server \
    && strip /usr/local/bin/mydevenv2-server

FROM ${UV_IMAGE} AS uv

# ─── Stage 3: the Python core, taken from the published image ───────────────
#
# Not built here. This is the *published public core image*, digest-pinned by
# the caller, and `/opt/vogt` is lifted out of it whole.
#
# It used to be a second `uv sync` of the same source, which meant the merged
# image contained a core that was built like the public one rather than being
# it. "The private path is the public path plus configuration" was then a
# claim about two builds agreeing, checkable by nobody. Now it is a digest.
#
# This only works because the public image bundles its own interpreter under
# `/opt/vogt/python` (see the root Dockerfile). A venv is not self-contained
# by default: `bin/python` links to whatever created it, so a venv built on
# Debian and copied onto Ubuntu is a directory of scripts whose interpreter
# does not exist — and the error names the script, not the missing
# interpreter. Verified by copying `/opt/vogt` from the published image into
# a bare `ubuntu` and running `vogt --version`.
#
# CORE_IMAGE has no default on purpose. A floating tag here would silently
# decouple the two halves of a commit-identified build; `build.yml` passes the
# digest its own core job just pushed.
ARG CORE_IMAGE
FROM ${CORE_IMAGE} AS core

# ─── Stage 4: the shipped image ─────────────────────────────────────────────
#
# Everything this stage used to install itself now lives in
# `engine/Dockerfile.pod` and arrives as `POD_BASE_IMAGE` (#184). It was 1344s
# of a 34 minute build and not one second of it depended on a line of this
# repository's source — see that file's header for the measurement and the
# variant/tagging scheme.
#
# `POD_BASE_IMAGE` is declared with the other base images at the top of this
# file, because an ARG referenced by a FROM has to be in the global scope —
# one declared down here would belong to the preceding stage and expand to
# nothing, which is a `FROM` with no argument rather than an error naming the
# variable.
#
# The pod base hands this stage over as `root`, on purpose: this stage installs
# into /usr/local before dropping to `sprooty` at the very end.
FROM ${POD_BASE_IMAGE}

# The agent CLIs stayed here rather than moving into the pod base, and that is a
# deliberate exception to "the toolchain lives in the base". They are pinned as
# ARGs and moved by Renovate (see the customManager in .github/renovate.json5,
# which reads the annotations). `latest` here means a stranger's publish decides
# what this image contains: 2.1.237 shipped `bin/claude.exe` as a shell stub and
# turned a green build red with no change on our side. It also means two builds
# of the same commit are different images, which defeats a commit-identified
# `dev-<sha>` (NFR-C3). This is the `update_automation_gap` the product reports
# on other people's repositories (FR-D6).
#
# Pinning also makes them cache-safe under the layer cache `build.yml` now sets:
# the RUN line's inputs change exactly when a pin does, so a warm cache reuses
# it only while the versions are genuinely unchanged — which is what a pin
# means. No day-bucket is needed; a reproducible pin is the stronger property.
#
# It goes *before* the two halves are copied in: the server binary changes on
# every commit, so every layer after that COPY is rebuilt every time regardless,
# and anything placed below it inherits that rate.
ARG INSTALL_AI_CLIENTS
ARG INSTALL_THECLAWBAY
# --prefix=/usr/local (not the ambient $NPM_CONFIG_PREFIX, which points at
# /home/sprooty/.npm-global) so these survive the runtime /home/sprooty bind
# mount — same reasoning as the system pnpm install in the base. The dev pod is
# the trusted isolation boundary, so install the real Codex entry point under
# libexec and expose the codex-full-access wrapper as `codex`. This overrides
# persisted per-user workspace-write/network restrictions for every repo under
# ~/Working.
#
# `theclawbay` is here for the reason the other two already are, learned the
# hard way (WI-17): it was never installed by any image and lived only in a
# persisted $HOME, so binding `vogt-dev` to a different home volume took it
# away and nothing was red.
COPY --chmod=755 engine/deploy/codex-full-access.sh /usr/local/bin/codex-full-access
# renovate: datasource=npm depName=@openai/codex
ARG CODEX_VERSION=0.148.0
# renovate: datasource=npm depName=@anthropic-ai/claude-code
ARG CLAUDE_CODE_VERSION=2.1.236
# renovate: datasource=npm depName=theclawbay
ARG THECLAWBAY_VERSION=0.6.13
# One `npm install -g` for whichever packages are wanted, not one per package:
# a second global install into the same prefix reconciles the whole tree for no
# benefit. No `sudo`: the pod base hands this stage over as root.
RUN pkgs=""; \
    if [ "$INSTALL_AI_CLIENTS" = "true" ]; then \
        pkgs="$pkgs @openai/codex@${CODEX_VERSION}"; \
        pkgs="$pkgs @anthropic-ai/claude-code@${CLAUDE_CODE_VERSION}"; \
    fi; \
    if [ "$INSTALL_THECLAWBAY" = "true" ]; then \
        pkgs="$pkgs theclawbay@${THECLAWBAY_VERSION}"; \
    fi; \
    if [ -n "$pkgs" ]; then \
        npm install -g --prefix=/usr/local $pkgs; \
    fi \
    && if [ "$INSTALL_AI_CLIENTS" = "true" ]; then \
        install -d /usr/local/libexec \
        && codex_target="$(readlink -f /usr/local/bin/codex)" \
        && ln -s "$codex_target" /usr/local/libexec/codex-real \
        && ln -sfn /usr/local/bin/codex-full-access /usr/local/bin/codex ; \
    fi

# Cadastre is a separate product. The private estate can opt into its bridge,
# but neither a normal engine build nor the public core image installs it — so
# it is gated here rather than baked into the shared pod base, whose `lean`
# variant also serves main's stack, which does not carry cadastre. pip3 is
# provided by the pod base.
ARG INSTALL_CADASTRE_MCP
RUN if [ "$INSTALL_CADASTRE_MCP" = "true" ]; then \
        pip3 install --break-system-packages --no-cache-dir "cadastre[mcp-client]"; \
    fi

# The server binary (built in stage 2 with the web bundle embedded; cache
# mounts in stage 2 mean we have to copy out of /usr/local/bin, not /app).
COPY --from=server-build /usr/local/bin/mydevenv2-server /usr/local/bin/mydevenv2-server
RUN chmod +x /usr/local/bin/mydevenv2-server

# The other half. `/opt/vogt` carries the standalone interpreter *and* the
# venv built against it — see stage 3; splitting them here is how you get a
# `vogt` that reports its own script as missing.
#
# Symlinked onto PATH rather than prepending the venv's `bin` to it. That
# directory also holds `python` and `python3`, and a dev pod whose `python3`
# is silently the core's private interpreter is a pod where every agent's
# `pip install` goes somewhere surprising. Three names, no PATH surgery — and
# `vogt` on PATH is also what makes the core reachable from a session inside
# this container, which is the whole point of merging the trees.
COPY --from=core /opt/vogt /opt/vogt
RUN ln -s /opt/vogt/.venv/bin/vogt /usr/local/bin/vogt \
    && ln -s /opt/vogt/.venv/bin/vogt-mcp /usr/local/bin/vogt-mcp \
    && ln -s /opt/vogt/.venv/bin/vogt-mcp-remote /usr/local/bin/vogt-mcp-remote \
    && vogt --version

# The core's data directory (declared.sqlite3, observed.sqlite3, backups).
# Owned by the pod's user rather than root:0 group-writable as in the
# core-only image: that image is runnable as any uid because a deployer picks
# one, whereas this one has already decided — the whole image is built around
# uid ${SPROOTY_UID} owning /home/sprooty. A fresh named volume is seeded from
# this ownership, so getting it wrong here surfaces on volume *recreation*,
# which usually means during a restore.
#
# Redeclared here because the stage that creates `sprooty` is now the pod base.
# They must agree with the uid/gid that base created `sprooty` at — the pod
# base defaults them to the same 1000/1000.
ARG SPROOTY_UID=1000
ARG SPROOTY_GID=1000
RUN install -d -o ${SPROOTY_UID} -g ${SPROOTY_GID} -m 0750 /var/lib/vogt

# Runtime scripts. Agent-specific CLIs are optional; these helpers only broker
# credentials on demand for neutral tools already present in the image.
COPY engine/deploy/entrypoint.sh /usr/local/bin/mydevenv2-entrypoint
COPY engine/deploy/agent-auth.sh /usr/local/bin/mydevenv2-agent-auth
COPY engine/deploy/cadastre-mcp-auth.sh /usr/local/bin/mydevenv2-cadastre-mcp
COPY engine/deploy/vogt-mcp-auth.sh /usr/local/bin/mydevenv2-vogt-mcp
COPY engine/deploy/mcp-bootstrap.sh /usr/local/bin/mydevenv2-mcp-bootstrap
COPY engine/deploy/git-askpass.sh /usr/local/bin/mydevenv2-git-askpass
COPY engine/deploy/rust-analyzer-mcp.sh /usr/local/bin/mydevenv2-rust-analyzer-mcp
RUN chmod +x \
    /usr/local/bin/mydevenv2-entrypoint \
    /usr/local/bin/mydevenv2-agent-auth \
    /usr/local/bin/mydevenv2-cadastre-mcp \
    /usr/local/bin/mydevenv2-vogt-mcp \
    /usr/local/bin/mydevenv2-mcp-bootstrap \
    /usr/local/bin/mydevenv2-git-askpass \
    /usr/local/bin/mydevenv2-rust-analyzer-mcp

# One port. vogt-core listens on loopback inside this namespace and is not
# exposed here, because there is nothing to expose it to: NFR-D11 makes the
# engine the only way in.
EXPOSE 8910
VOLUME ["/home/sprooty/Working"]
VOLUME ["/var/lib/vogt"]

USER sprooty
WORKDIR /home/sprooty/Working

ENV MYDEVENV2_BIND=0.0.0.0:8910
ENV RUST_LOG=info
# An allocation, so it carries a default (NFR-D2 as revised r4): it names a
# directory inside this image, which is the one thing the image does know.
ENV VOGT_DATA_DIR=/var/lib/vogt
# `VOGT_CORE_URL` is deliberately *not* defaulted here. It is what decides
# whether this container runs a core at all, and an image that always tries to
# is an image that cannot be MyDevEnv2 on its own — which FR-E9 requires it to
# stay. The compose file turns the core on; see deploy/entrypoint.sh.

ENTRYPOINT ["/usr/local/bin/mydevenv2-entrypoint"]
CMD []
