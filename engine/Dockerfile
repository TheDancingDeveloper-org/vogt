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
#   1. web-build     — pnpm install + vite build of web/
#   2. chef/planner  — cargo-chef recipe: the dependency graph, without sources
#      server-build  — deps compiled as their own layer, then the workspace
#                      built on top with the bundle embedded
#   3. core-build    — uv sync of the Python core into a self-contained venv
#   4. runtime       — `FROM ${POD_BASE_IMAGE}` plus both halves + entrypoint
#
# Stage order matters in one place: rust-embed reads `web/dist` at *compile*
# time, so the web bundle is a build input to the Rust binary rather than a
# runtime asset. Stage 2's *final* step cannot start before stage 1 finishes —
# but its dependency layer can and does, which is the point of the split.
#
# The TOOLING.md toolchain — Node, the Rust dev toolchain, sway, selkies,
# Java/Gradle, the Android SDK, Flutter — is no longer installed here. It is
# `engine/Dockerfile.pod`, published as `vogt-pod-base:{lean,full}-*` and passed
# in as `POD_BASE_IMAGE`. It was 22 of this image's 34 build minutes and none of
# it depends on this repository's source; that file's header carries the
# measurement.
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
#
# The three Docker Hub library bases are named through this organisation's
# GHCR mirror rather than through Docker Hub (#33). Builds kept failing at
# `FROM` with 429: a self-hosted runner is one IP for every build this
# repository makes, and authenticating measurably made it worse, because
# the available credential is estate-wide and its bucket is empty exactly
# when the estate is busy. `.github/workflows/mirror-base-images.yml` copies
# each manifest list into GHCR verbatim — the index survives, so these still
# resolve per-platform — and runs as a prerequisite of every build, so a
# Renovate bump is mirrored before anything tries to build from it.
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
# The runtime base, named once because the core's venv is built on it too —
# see stage 3 for why that matters more than it looks.
ARG RUNTIME_IMAGE=ghcr.io/thedancingdeveloper-org/vogt-base/ubuntu:26.04
# uv builds the core's virtualenv from the committed lockfile, so the image
# contains exactly what CI tested (NFR-Q5). Same pin as the root Dockerfile;
# they should move together.
ARG UV_IMAGE=ghcr.io/astral-sh/uv:0.9.18
# The dev-pod toolchain base — everything stage 4 used to install itself, and
# 22 of this image's 34 build minutes. Built and published by
# `.github/workflows/pod-base.yml` from `engine/Dockerfile.pod`; see either for
# the variants and the tagging.
#
# CI always overrides this with a **digest**, resolved by that workflow. The
# default here names a moving tag and is a readable placeholder for a local
# build, not a supported way to produce a shipped image — which is why the
# pin lives in the workflow rather than in this line, unlike the mirrored
# bases above whose digests Renovate maintains here.
ARG POD_BASE_IMAGE=ghcr.io/thedancingdeveloper-org/vogt-pod-base:lean
# The interpreter the core runs on. Requested from uv rather than from the
# distribution: `requires-python = ">=3.11"` is satisfied by whatever Ubuntu
# ships, but "whatever Ubuntu ships" is not a version this repository chose,
# and it changes under us on a base-image bump.
ARG CORE_PYTHON=3.13
# The Android SDK and Flutter pins moved to `engine/Dockerfile.pod` along with
# the installs that read them. They are the pod's toolchain, not this image's
# content, and a pin that lives away from its only consumer is a pin that goes
# stale without anything noticing.
# Off by default (prod). `.github/workflows/build.yml` passes
# INSTALL_AI_CLIENTS=true when the ref is the `dev` branch — the branch that
# also produces the `:dev` and `dev-<sha>` tags — to bake in codex + claude
# for pre-prod trial. See AGENTS.md "Codex and Claude are deliberately not
# installed by container bootstrap" for why prod stays opt-in/user-managed
# for these two specifically. Intentionally unpinned (always latest at build
# time) per user direction — unlike every other tool in this file, these two
# are expected to move fast and dev is where that churn should be absorbed
# first.
#
# This comment used to name `.woodpecker/server.yml build-and-push-dev`. That
# pipeline was retired in the move to Actions and nothing took over the build
# arg, so the default stood and `vogt-dev` shipped with neither client (#23).
# A default is only safe while something is documented to override it — and
# the something has to exist.
ARG INSTALL_AI_CLIENTS=false
# Flutter is also dev-only, and is now the thing that separates the two
# published variants of the pod base rather than a build arg here: `dev` is
# built on `vogt-pod-base:full-*`; main and every release on `:lean-*`.
# See `engine/Dockerfile.pod`.

# ─── Stage 1: web bundle ────────────────────────────────────────────────────
FROM ${NODE_IMAGE} AS web-build
WORKDIR /app/web
COPY web/package.json web/pnpm-lock.yaml ./
RUN npm install -g pnpm && pnpm install --frozen-lockfile
COPY web/ ./
RUN pnpm build

# ─── Stage 2: rust binary with embedded web/ ────────────────────────────────
#
# Split into a dependency bake and a workspace build via cargo-chef, and that
# split is the whole point of this stage's shape.
#
# What it replaces: one `cargo build --release` behind
# `--mount=type=cache,target=/app/engine/target`. Two things were wrong with
# that. The first is that a BuildKit cache mount is *builder-local state* —
# it lives in the buildkitd instance, not in a layer — so it is invisible to
# `cache-from`/`cache-to` and cannot cross a runner. On a self-hosted fleet
# where a job lands on whichever node-b worker is free, that mount was empty
# on essentially every run: 458 crates compiled from nothing, every time,
# with `codegen-units = 1` and thin LTO on top.
#
# The second is subtler and is the one that actually surprises people.
# `server/src/assets.rs` embeds `../../web/dist/` with rust-embed, which reads
# it at *compile* time — so the bundle is a build input to the crate. The old
# stage copied it in before the single build step, which meant a stylesheet
# change invalidated the layer and recompiled all 458 dependencies to embed a
# different CSS file. Most of the Rust rebuilds this repository was paying for
# were not caused by Rust changing.
#
# Both are fixed by making the dependency compile a real layer keyed on
# nothing but `Cargo.toml`/`Cargo.lock`. Layers replicate through
# `cache-from`/`cache-to` and therefore across the fleet; `web/dist` and the
# crate sources are copied in *after* it, so a web-only or `server/src`-only
# change recompiles two crates instead of four hundred and sixty.
#
# The cache mounts are gone rather than kept alongside, deliberately: a target
# directory that is a cache mount is not in the layer, so keeping them would
# have made the cook stage produce an empty layer and quietly restored the
# original behaviour with more moving parts.
FROM ${RUST_IMAGE} AS chef
# Built from source rather than pulled from `lukemathwalker/cargo-chef`, which
# is the usual advice: that image is on Docker Hub, and #33 is the whole reason
# nothing in this repository builds `FROM` there. Compiling it costs a minute
# on a cold cache and nothing on a warm one — this layer's inputs never change.
#
# Pinned, because cargo-chef parses the manifests and emits the recipe that
# decides what the dependency layer contains: an unpinned version is a silent
# input to what gets compiled.
RUN cargo install cargo-chef --locked --version 0.1.78
# The layout inside this stage mirrors the repository's, because rust-embed
# resolves `../../web/dist/` against the *crate* directory. Build the workspace
# at /app/engine and the bundle lands where the macro already looks: put the
# crate at /app/server instead and it would read /web/dist, which is nothing.
WORKDIR /app/engine

FROM chef AS planner
COPY engine/Cargo.toml engine/Cargo.lock ./
COPY engine/contract/Cargo.toml ./contract/Cargo.toml
COPY engine/contract/src ./contract/src
COPY engine/server/Cargo.toml ./server/Cargo.toml
COPY engine/server/src ./server/src
RUN cargo chef prepare --recipe-path /recipe.json

FROM chef AS server-build
# The recipe is a normalised description of the dependency graph only: it does
# not change when a source file does, which is what makes the layer below
# survive an ordinary commit.
COPY --from=planner /recipe.json /recipe.json
# The whole workspace, not `-p mydevenv2-server`: `contract` is a path
# dependency of `server`, so the dependency set is the same either way, and
# cooking the workspace keeps this line independent of which binaries the
# workspace grows.
RUN cargo chef cook --release --recipe-path /recipe.json

# Only now the things that change on nearly every commit.
COPY engine/Cargo.toml engine/Cargo.lock ./
COPY engine/contract/Cargo.toml ./contract/Cargo.toml
COPY engine/contract/src ./contract/src
COPY engine/server/Cargo.toml ./server/Cargo.toml
COPY engine/server/src ./server/src
COPY --from=web-build /app/web/dist /app/web/dist
RUN cargo build --release -p mydevenv2-server \
    && cp target/release/mydevenv2-server /usr/local/bin/mydevenv2-server \
    && strip /usr/local/bin/mydevenv2-server
FROM ${UV_IMAGE} AS uv

# ─── Stage 3: the Python core's virtualenv ──────────────────────────────────
#
# Built on the runtime base rather than on `python:3.13-slim` (which is what
# the root Dockerfile uses, because there its runtime stage is that same
# image). A venv is not a self-contained thing: `bin/python` is a link to the
# interpreter that created it, so a venv built on Debian and copied onto Ubuntu
# is a directory full of scripts whose interpreter does not exist — and the
# error names the script, not the missing interpreter.
#
# Two ways out; this takes the second. Copying the base image's `/usr/local`
# python across works but silently entangles two distributions' libc and
# OpenSSL. Instead uv downloads a standalone CPython into /opt/vogt/python and
# builds the venv against it, so `COPY /opt/vogt` carries the interpreter, the
# venv, and every console script's absolute shebang as one relocation-free
# unit. The install directory is fixed rather than defaulted for exactly that
# reason: the path in this stage has to be the path in the runtime stage.
FROM ${RUNTIME_IMAGE} AS core-build
ARG CORE_PYTHON
COPY --from=uv /uv /usr/local/bin/uv
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
ENV UV_COMPILE_BYTECODE=1 \
    UV_LINK_MODE=copy \
    UV_PYTHON=${CORE_PYTHON} \
    UV_PYTHON_DOWNLOADS=automatic \
    UV_PYTHON_INSTALL_DIR=/opt/vogt/python \
    UV_PROJECT_ENVIRONMENT=/opt/vogt/.venv
WORKDIR /src
# Dependencies first, so a source-only change does not re-resolve them.
COPY pyproject.toml uv.lock README.md LICENSE ./
RUN --mount=type=cache,target=/root/.cache/uv \
    uv sync --locked --no-install-project --no-dev
COPY src/ ./src/
# `--no-editable` because uv.lock records the project as an editable source.
# An editable install is a `.pth` file pointing back at /src, which does not
# exist in the runtime stage — the venv would import every dependency and then
# fail on `No module named 'vogt'`.
RUN --mount=type=cache,target=/root/.cache/uv \
    uv sync --locked --no-dev --no-editable \
    && /opt/vogt/.venv/bin/vogt --version

# ─── Stage 4: the shipped image ─────────────────────────────────────────────
#
# Everything this stage used to install itself now lives in
# `engine/Dockerfile.pod` and arrives as `POD_BASE_IMAGE`. It was 1344s of a
# 34 minute build and not one second of it depended on a line of this
# repository's source — see that file's header for the measurement and the
# variant/tagging scheme.
#
# `POD_BASE_IMAGE` is declared with the other base images at the top of this
# file, because an ARG referenced by a FROM has to be in the global scope —
# one declared down here would belong to the preceding stage and expand to
# nothing, which is a `FROM` with no argument rather than an error naming the
# variable.
FROM ${POD_BASE_IMAGE}

# The agent CLIs stayed here rather than moving into the base, and that is a
# deliberate exception to "the toolchain lives in the base".
#
# These three are the one part of the toolchain documented as intentionally
# unpinned — always latest at build time, because dev is where their churn
# should be absorbed first (see the `INSTALL_AI_CLIENTS` note at the top of
# this file, and #23 for what happened when nothing overrode the default).
# A weekly-rebuilt base image would have turned that into "latest as of last
# Monday" without anybody deciding to, and turning on a build cache would
# have frozen them outright: the RUN below is textually identical from one
# commit to the next, so a warm cache would reuse it forever.
#
# `AI_CLIENTS_EPOCH` is what keeps the promise honest. CI sets it to the
# current date, so the layer is reused within a day and rebuilt across one:
# fresh clients daily, for ~110s once a day.
#
# It goes here, *before* the two halves are copied in, and that ordering is
# the whole reason the 110s is once a day rather than once a commit. The
# server binary changes on every commit, so every layer after that COPY is
# rebuilt every time regardless; anything placed below it inherits that rate.
# Above it, this layer's only input is the epoch.
#
# Setting the arg to a fixed string instead pins the clients to whatever a
# cache holds — the right behaviour for a rebuild meant to reproduce an
# earlier image, the wrong one for CI, which is why the default is `pinned`
# and CI overrides it rather than the other way round.
ARG INSTALL_AI_CLIENTS
ARG AI_CLIENTS_EPOCH=pinned
# --prefix=/usr/local (not the ambient $NPM_CONFIG_PREFIX, which points at
# /home/sprooty/.npm-global) so these survive the runtime /home/sprooty bind
# mount — same reasoning as the system pnpm install in the base. The dev pod
# is the trusted isolation boundary, so install the real Codex entry point
# under libexec and expose the codex-full-access wrapper as `codex`. This
# overrides persisted per-user workspace-write/network restrictions for every
# repo under ~/Working.
#
# `theclawbay` is here for the reason the other two already are, learned the
# hard way: it was never installed by any image, in MyDevEnv2 or here. It was
# an npm global somebody put in the persisted $HOME, so `vogt-dev` — bound to
# a different home volume than `mydevenv2-dev` — simply did not have it, and
# nothing was red. A tool the pod is expected to carry belongs in the image
# under /usr/local, where the runtime bind mount cannot take it away. That it
# also installs *into* $HOME is unchanged and still handled by the PATH
# ordering in the base; this only fixes where `theclawbay` itself lives.
#
# No `sudo` any more: the base hands this stage over as root, where the
# original ran as `sprooty` and escalated for every line of it.
COPY --chmod=755 engine/deploy/codex-full-access.sh /usr/local/bin/codex-full-access
RUN if [ "$INSTALL_AI_CLIENTS" = "true" ]; then \
        echo "installing agent CLIs (epoch ${AI_CLIENTS_EPOCH})" \
        && npm install -g --prefix=/usr/local @openai/codex @anthropic-ai/claude-code theclawbay \
        && install -d /usr/local/libexec \
        && codex_target="$(readlink -f /usr/local/bin/codex)" \
        && ln -s "$codex_target" /usr/local/libexec/codex-real \
        && ln -sfn /usr/local/bin/codex-full-access /usr/local/bin/codex ; \
    fi

# The server binary, built in stage 2 with the web bundle embedded. Copied
# from /usr/local/bin rather than from the target directory because that is
# where stage 2 installs it; it stays that way now the cache mounts are gone,
# since a stage boundary is a cleaner contract than a build-profile path.
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
COPY --from=core-build /opt/vogt /opt/vogt
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
# Redeclared here because the stage that used to declare them is now the pod
# base. They must agree with the uid/gid that base created `sprooty` at —
# `tests/test_deploy.py` asserts on ownership rather than trusting that they do.
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
