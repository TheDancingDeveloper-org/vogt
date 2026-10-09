# syntax=docker/dockerfile:1
#
# Vogt — the merged image.
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
#   1a. demo-web / demo-runtime — metadata-only augmentation + static demo image
#   2. server-build — cargo build --release of engine/server, embedding the bundle
#   3. core — the published public core image, lifted whole
#   4. runtime      — `FROM ${POD_BASE_IMAGE}` plus both halves + entrypoint
#
# Stage order matters in one place: rust-embed reads `web/dist` at *compile*
# time, so the web bundle is a build input to the Rust binary rather than a
# runtime asset. Stage 2 cannot start before stage 1 finishes.
#
# The dev-pod toolchain — Node, the Rust dev toolchain, sway, selkies,
# Java/Gradle, the Android SDK, Flutter — is no longer installed here. It
# is `engine/Dockerfile.pod`, published as `vogt-pod-base:{lean,full}-*` by
# `.github/workflows/pod-base.yml` and passed in as `POD_BASE_IMAGE`. It was 22
# of this image's 34 build minutes and none of it depends on this repository's
# source; that file's header carries the measurement.
#
# Why the core-only image at the repository root still exists: it is the
# hardened artifact (`read_only`, `cap_drop: [ALL]`, runnable as any uid) that
# Topology A and the standalone core deployments use, and vogt-core is kept
# independently runnable on purpose. This image cannot be that: it
# is a development pod — sudo, agent CLIs, a writable home — and collapsing
# the two would quietly delete vogt's containment story under an unchanged
# image name.
#
# Run as one stack by ../deploy/stack.compose.yml.

# The two Docker Hub library bases used by stages 1 and 2 are named through
# this organisation's GHCR mirror rather than through Docker Hub. Builds
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
# The static demo's *runtime* base, deliberately not `NODE_IMAGE`. Building the
# PWA needs a toolchain; serving the built bytes does not — `demo-server.mjs`
# imports only `node:` builtins. `node:22-bookworm` derives from
# `buildpack-deps` and carries 413 packages including compilers and `-dev`
# headers, every one of which is attack surface the demo never uses and a
# fatal-gated Trivy finding waiting to be published against it. The `rm -rf npm`
# in the demo stage is the scar from the last round of that;
# `libexpat1`/`libexpat1-dev` (CVE-2026-56408) was the next, and it failed
# `dev`'s build on four consecutive pushes while nothing in the tree had
# changed. The slim base carries 88 packages and neither of those.
ARG DEMO_RUNTIME_IMAGE=ghcr.io/thedancingdeveloper-org/vogt-base/node:22-bookworm-slim
ARG RUST_IMAGE=ghcr.io/thedancingdeveloper-org/vogt-base/rust:1-bookworm
# uv builds the core's virtualenv from the committed lockfile, so the image
# contains exactly what CI tested. Same pin as the root Dockerfile;
# they should move together.
ARG UV_IMAGE=ghcr.io/astral-sh/uv:0.9.18
# The dev-pod toolchain base — everything stage 4 used to install itself, and
# 22 of this image's 34 build minutes. Built and published by
# `.github/workflows/pod-base.yml` from `engine/Dockerfile.pod`; see either for
# the variants and the tagging.
#
# CI always overrides this with a **digest**, resolved by that workflow. The
# default here names a moving tag and is a readable placeholder for a local
# build, not a supported way to produce a shipped image — which is why the pin
# lives in the workflow rather than in this line, unlike the mirrored bases
# above whose digests Renovate maintains here.
ARG POD_BASE_IMAGE=ghcr.io/thedancingdeveloper-org/vogt-pod-base:lean
# The published public core image, lifted whole in stage 3. The default names
# the public `:latest` tag so `docker build -f engine/Dockerfile .` works from a
# clean clone with no `--build-arg`; it is a readable placeholder for a local
# build, not a supported way to produce a shipped image. CI always overrides it
# with a **digest** so a commit-identified build pins both halves — see stage 3
# and `.github/workflows/build.yml`, exactly as `POD_BASE_IMAGE` above.
ARG CORE_IMAGE=ghcr.io/thedancingdeveloper-org/vogt:latest
ARG VOGT_SOURCE_REF=local
ARG VOGT_SOURCE_SHA=0000000000000000000000000000000000000000
ARG VOGT_PRODUCT_VERSION=local/dev
# Off by default (prod). `.github/workflows/build.yml` passes
# INSTALL_AI_CLIENTS=true when the ref is the `dev` branch — the branch that
# also produces the `:dev` and `dev-<sha>` tags — to bake in codex + claude
# for pre-prod trial. See AGENTS.md "Codex and Claude are deliberately not
# installed by container bootstrap" for why prod stays opt-in/user-managed
# for these two specifically.
#
# This comment used to name `.woodpecker/server.yml build-and-push-dev`. That
# pipeline was retired in the move to Actions and nothing took over the build
# arg, so the default stood and `vogt-dev` shipped with neither client.
# A default is only safe while something is documented to override it — and
# the something has to exist.
ARG INSTALL_AI_CLIENTS=false
# Flutter is dev-only, and is now the thing that separates the two published
# variants of the pod base rather than a build arg here: `dev` is built
# on `vogt-pod-base:full-*`; main and every release on `:lean-*`. See
# `engine/Dockerfile.pod`.
# ─── Stage 1: web bundle ────────────────────────────────────────────────────
FROM ${NODE_IMAGE} AS web-build
ARG VOGT_SOURCE_REF
ARG VOGT_SOURCE_SHA
ARG VOGT_PRODUCT_VERSION
WORKDIR /app/web
# pnpm-workspace.yaml carries the `overrides` the lockfile is resolved against
# (pnpm 11 moved them out of package.json). Without it here the frozen install
# sees no overrides while the lockfile encodes them and aborts with
# ERR_PNPM_LOCKFILE_CONFIG_MISMATCH. The full `COPY web/ ./` below re-copies it,
# but the install one line down needs it first.
COPY web/package.json web/pnpm-workspace.yaml web/pnpm-lock.yaml ./
RUN npm install -g pnpm && pnpm install --frozen-lockfile
COPY web/ ./
RUN set -eu; \
    product_version="${VOGT_PRODUCT_VERSION#v}"; \
    export VOGT_SOURCE_REF="$VOGT_SOURCE_REF" \
        VOGT_SOURCE_SHA="$VOGT_SOURCE_SHA" \
        VOGT_PRODUCT_VERSION="$product_version"; \
    pnpm build

# ─── Public demo target ────────────────────────────────────────────────────
# `demo-runtime` branches from the exact `web-build` stage the normal engine
# embeds. It verifies those already-built asset hashes before adding the two
# demo-only documents. CI builds the normal stack first, then this target from
# that stack's exported cache, so parity is a shared layer rather than two
# compilers being expected to agree.
FROM web-build AS demo-web
ARG VOGT_SOURCE_REF
ARG VOGT_SOURCE_SHA
ARG VOGT_PRODUCT_VERSION
RUN set -eu; \
    product_version="${VOGT_PRODUCT_VERSION#v}"; \
    export VOGT_SOURCE_REF="$VOGT_SOURCE_REF" \
        VOGT_SOURCE_SHA="$VOGT_SOURCE_SHA" \
        VOGT_PRODUCT_VERSION="$product_version"; \
    pnpm demo:augment

FROM ${DEMO_RUNTIME_IMAGE} AS demo-runtime
WORKDIR /app
COPY --from=demo-web /app/web/dist /app/dist
COPY engine/deploy/demo-server.mjs /app/demo-server.mjs
# A restrictive checkout umask may make COPY preserve mode 0600.  Normalize
# the one executable input before dropping to the unprivileged node user.
#
# The demo server imports only `node:` builtins (see demo-server.mjs), so the
# npm CLI the base image bundles is dead weight — and its own bundled
# dependencies (tar, brace-expansion, pacote, sigstore, …) are what the fatal
# Trivy gate flags as CRITICAL/HIGH. Removing npm/npx clears every one of those
# findings without touching anything the demo runtime uses. The slim base still
# ships npm, so this stays load-bearing.
#
# The base is a mirrored tag, and a mirrored tag does not refresh once copied,
# so Debian security fixes published after the mirror (libpcre2
# CVE-2026-103111, fixed in 10.42-1+deb12u2) never reach this image on their
# own. Apply them here so the Trivy gate sees a patched OS layer.
RUN apt-get update \
    && apt-get -y --no-install-recommends upgrade \
    && rm -rf /var/lib/apt/lists/* \
    && chmod 0444 /app/demo-server.mjs \
    && rm -rf /usr/local/lib/node_modules/npm \
              /usr/local/bin/npm \
              /usr/local/bin/npx
USER node
EXPOSE 8910
ENV DEMO_BIND=0.0.0.0 \
    DEMO_PORT=8910 \
    NODE_ENV=production
ENTRYPOINT ["node", "/app/demo-server.mjs"]

# ─── Stage 2: rust binary with embedded web/ ────────────────────────────────
FROM ${RUST_IMAGE} AS server-build
ARG VOGT_SOURCE_REF
ARG VOGT_SOURCE_SHA
ARG VOGT_PRODUCT_VERSION
# The layout inside this stage mirrors the repository's, because rust-embed
# resolves `../../web/dist/` against the *crate* directory. Build the workspace
# at /app/engine and the bundle lands where the macro already looks: put the
# crate at /app/server instead and it would read /web/dist, which is nothing.
WORKDIR /app/engine
COPY engine/Cargo.toml engine/Cargo.lock ./
COPY engine/contract/Cargo.toml ./contract/Cargo.toml
COPY engine/contract/src ./contract/src
COPY engine/server/Cargo.toml ./server/Cargo.toml
COPY engine/server/build.rs ./server/build.rs
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
    VOGT_SOURCE_REF="$VOGT_SOURCE_REF" \
    VOGT_SOURCE_SHA="$VOGT_SOURCE_SHA" \
    VOGT_PRODUCT_VERSION="${VOGT_PRODUCT_VERSION#v}" \
    cargo build --release -p vogt-engine-server \
    && cp target/release/vogt-engine /usr/local/bin/vogt-engine \
    && strip /usr/local/bin/vogt-engine

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
# CORE_IMAGE defaults to the public `:latest` tag (declared at the top of this
# file) so a bare `docker build` works from a clean clone. A floating tag would
# silently decouple the two halves of a commit-identified build, so `build.yml`
# always passes the digest its own core job just pushed — the default is the
# local-build convenience, never the shipped path.
ARG CORE_IMAGE
FROM ${CORE_IMAGE} AS core

# ─── Stage 4: the shipped image ─────────────────────────────────────────────
#
# Everything this stage used to install itself now lives in
# `engine/Dockerfile.pod` and arrives as `POD_BASE_IMAGE`. It was 1344s
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

LABEL org.opencontainers.image.title="vogt-stack" \
      org.opencontainers.image.description="Vogt: core, session engine, PWA and agent CLIs in one container" \
      org.opencontainers.image.source="https://github.com/TheDancingDeveloper-org/vogt" \
      org.opencontainers.image.licenses="AGPL-3.0-only"

# The agent CLIs live here rather than in the pod base, and that is a
# deliberate exception to "the toolchain lives in the base". They are pinned in
# `engine/agent-versions.env` and moved by Renovate (see the customManager in
# `.github/renovate.json5`). `latest` here means a stranger's publish decides
# what this image contains: 2.1.237 shipped `bin/claude.exe` as a shell stub and
# turned a green build red with no change on our side. It also means two builds
# of the same commit are different images, which defeats a commit-identified
# `dev-<sha>`. This is the `update_automation_gap` the product reports
# on other people's repositories.
#
# Pinning also makes them cache-safe under the layer cache `build.yml` sets:
# the RUN line's inputs change exactly when a pin does, so a warm cache reuses
# it only while the versions are genuinely unchanged — which is what a pin
# means. No day-bucket is needed; a reproducible pin is the stronger property.
#
# It goes *before* the two halves are copied in: the server binary changes on
# every commit, so every layer after that COPY is rebuilt every time regardless,
# and anything placed below it inherits that rate.
COPY engine/agent-versions.env /usr/local/share/vogt/agent-versions.env

# The Go toolchain (WI-951), so a session can build Go — an agent CLI that is
# a Go program among it — without a hand-unpacked tarball in the persisted
# home that nothing declares and no other pod has. Pinned in
# `engine/agent-versions.env` with its release checksums, unpacked outside
# /home/sprooty (which the runtime bind-mounts over), and linked into
# /usr/local/bin so the runtime pin below finds an image copy to fall back to,
# exactly as it does for the npm CLIs. `GOTOOLCHAIN=local` keeps it the
# toolchain that runs: a `go.mod` asking for a newer one fails the build by
# name instead of fetching an unpinned toolchain into the module cache.
ARG GO_VERSION
ENV GOTOOLCHAIN=local
RUN set -eu; \
    . /usr/local/share/vogt/agent-versions.env; \
    go_version="${GO_VERSION:-${VOGT_GO_VERSION}}"; \
    case "$(dpkg --print-architecture)" in \
        amd64) arch=amd64; go_sum="$VOGT_GO_SHA256_LINUX_AMD64" ;; \
        arm64) arch=arm64; go_sum="$VOGT_GO_SHA256_LINUX_ARM64" ;; \
        *) echo "no pinned Go for $(dpkg --print-architecture)" >&2; exit 1 ;; \
    esac; \
    if [ "$go_version" != "$VOGT_GO_VERSION" ]; then \
        echo "GO_VERSION=$go_version has no recorded checksum; edit engine/agent-versions.env instead" >&2; exit 1; \
    fi; \
    curl -fsSL "https://go.dev/dl/go${go_version}.linux-${arch}.tar.gz" -o /tmp/go.tar.gz; \
    echo "$go_sum  /tmp/go.tar.gz" | sha256sum -c -; \
    rm -rf /usr/local/go; \
    tar -xzf /tmp/go.tar.gz -C /usr/local; \
    rm -f /tmp/go.tar.gz; \
    ln -sfn /usr/local/go/bin/go /usr/local/bin/go; \
    ln -sfn /usr/local/go/bin/gofmt /usr/local/bin/gofmt; \
    go version | grep -qF "go${go_version} "

ARG INSTALL_AI_CLIENTS
ARG CODEX_VERSION
ARG CLAUDE_CODE_VERSION
ARG OPENCODE_VERSION
ARG KLAUDIA_VERSION
# --prefix=/usr/local (not the ambient $NPM_CONFIG_PREFIX, which points at
# /home/sprooty/.npm-global) so these survive the runtime /home/sprooty bind
# mount — same reasoning as the system pnpm install in the base. The dev pod is
# the trusted isolation boundary, so install the real Codex entry point under
# libexec and expose the codex-full-access wrapper as `codex`. This overrides
# persisted per-user workspace-write/network restrictions for every repo under
# ~/Working.
COPY --chmod=755 engine/deploy/codex-full-access.sh /usr/local/bin/codex-full-access
# The driven-session permission policy (WI-926): auto-mode rules every
# engine-launched Claude session gets with --settings. A deployment replaces it
# with ENGINE_AGENT_CLAUDE_SETTINGS (docs/ENGINE.md, "Permission posture").
COPY engine/deploy/driven-session-settings.json /usr/local/share/vogt/driven-session-settings.json
# Its opencode half (WI-932), given as OPENCODE_CONFIG_CONTENT; replaced with
# ENGINE_AGENT_OPENCODE_CONFIG.
COPY engine/deploy/driven-session-opencode.json /usr/local/share/vogt/driven-session-opencode.json
# The pin below is the *baked baseline*. A deployment may name a different
# version per tool at runtime (`VOGT_CLAUDE_CODE_VERSION` and friends in the
# container's environment; `agent-clis.tools`, written below, is the
# table of tool → package → binary → variable the runtime scripts read, so
# they name no tool themselves): the entrypoint installs it into a versioned
# prefix under this root and flips a `current` symlink, so the CLI a session
# runs is decided at deploy time and changed with a restart rather than with
# an image build and a release cut. PATH puts the root's `bin/` ahead of
# /usr/local/bin — set here rather than in the pod base so it lands with this
# image and needs no base rebuild — and the image copy under /usr/local stays
# untouched as the fallback for an empty volume or a failed install. The
# directory is created below, once the runtime uid is known, so a named volume
# mounted over it inherits `sprooty` ownership.
ENV VOGT_AGENT_CLI_ROOT=/opt/vogt/agent-clis
ENV PATH=/opt/vogt/agent-clis/bin:$PATH
# One `npm install -g` for whichever packages are wanted, not one per package:
# a second global install into the same prefix reconciles the whole tree for no
# benefit. No `sudo`: the pod base hands this stage over as root.
#
# The install is then asked to prove itself, because `npm install -g` exiting 0
# is not evidence that `claude` runs. The published tarball ships
# `bin/claude.exe` as a ~500-byte shell stub; the real binary arrives from the
# `postinstall` script or a platform-native optional dependency, so
# `--ignore-scripts`, some pnpm configs, or a missing optional dep all yield a
# green build and a pod whose `claude` only prints an error. That is the
# packaging that broke 2.1.237 and held the pin at 2.1.236 for
# months; it has not changed. One `--version` makes every future Renovate bump
# of the pin self-verifying instead of hopeful.
RUN set -eu; \
    . /usr/local/share/vogt/agent-versions.env; \
    codex_version="${CODEX_VERSION:-${VOGT_CODEX_VERSION}}"; \
    claude_code_version="${CLAUDE_CODE_VERSION:-${VOGT_CLAUDE_CODE_VERSION}}"; \
    opencode_version="${OPENCODE_VERSION:-${VOGT_OPENCODE_VERSION}}"; \
    go_version="$(go version | sed -n 's/^go version go\([^ ]*\) .*/\1/p')"; \
    klaudia_version="${KLAUDIA_VERSION:-${VOGT_KLAUDIA_VERSION}}"; \
    printf 'codex=%s\nclaude-code=%s\nopencode=%s\ngo=%s\nklaudia=%s\n' \
        "$codex_version" "$claude_code_version" "$opencode_version" "$go_version" "$klaudia_version" \
        > /usr/local/share/vogt/agent-versions.resolved; \
    printf 'codex\t@openai/codex\tcodex\tVOGT_CODEX_VERSION\tnpm\nclaude-code\t@anthropic-ai/claude-code\tclaude\tVOGT_CLAUDE_CODE_VERSION\tnpm\nopencode\topencode-ai\topencode\tVOGT_OPENCODE_VERSION\tnpm\ngo\thttps://go.dev/dl\tgo\tVOGT_GO_VERSION\tgo-dist\nklaudia\t%s\tklaudia\tVOGT_KLAUDIA_VERSION\tgo-src\n' \
        "$VOGT_KLAUDIA_SOURCE" \
        > /usr/local/share/vogt/agent-clis.tools; \
    pkgs=""; \
    if [ "$INSTALL_AI_CLIENTS" = "true" ]; then \
        pkgs="$pkgs @openai/codex@${codex_version}"; \
        pkgs="$pkgs @anthropic-ai/claude-code@${claude_code_version}"; \
        pkgs="$pkgs opencode-ai@${opencode_version}"; \
        # The pod base's unpinned copy (its `curl | bash` installer) is
        # replaced by the pinned package; npm refuses to overwrite a file it
        # does not own (WI-929).
        rm -f /usr/local/bin/opencode; \
    fi; \
    if [ -n "$pkgs" ]; then \
        npm install -g --prefix=/usr/local $pkgs; \
    fi \
    && if [ "$INSTALL_AI_CLIENTS" = "true" ]; then \
        /usr/local/bin/claude --version | grep -qF "$claude_code_version" \
        || { echo "claude is not $claude_code_version — a stub install" >&2; exit 1; } \
        && /usr/local/bin/opencode --version | grep -qF "$opencode_version" \
        || { echo "opencode is not $opencode_version — the platform binary did not install" >&2; exit 1; } \
        && install -d /usr/local/libexec \
        && codex_target="$(readlink -f /usr/local/bin/codex)" \
        && ln -s "$codex_target" /usr/local/libexec/codex-real \
        && ln -sfn /usr/local/bin/codex-full-access /usr/local/bin/codex ; \
    fi

# Klaudia (WI-950), built from the pinned commit with the pinned Go: no
# registry publishes it. Its own layer, so a Klaudia pin moves without
# reinstalling the npm CLIs above. Only that commit is fetched, the build
# must reproduce it exactly (`-mod=readonly`, `-trimpath`, CGO off), and the
# module and build caches are dropped so the image carries the binary alone.
RUN set -eu; \
    if [ "$INSTALL_AI_CLIENTS" = "true" ]; then \
        . /usr/local/share/vogt/agent-versions.env; \
        klaudia_version="${KLAUDIA_VERSION:-${VOGT_KLAUDIA_VERSION}}"; \
        git init -q /tmp/klaudia; \
        git -C /tmp/klaudia fetch -q --depth 1 "$VOGT_KLAUDIA_SOURCE" "$klaudia_version"; \
        git -C /tmp/klaudia checkout -q --detach FETCH_HEAD; \
        test "$(git -C /tmp/klaudia rev-parse HEAD)" = "$klaudia_version"; \
        (cd /tmp/klaudia && CGO_ENABLED=0 GOFLAGS=-mod=readonly \
            GOMODCACHE=/tmp/gomod GOCACHE=/tmp/gocache \
            go build -trimpath -o /usr/local/bin/klaudia ./cmd/klaudia); \
        GOMODCACHE=/tmp/gomod go clean -modcache; \
        rm -rf /tmp/klaudia /tmp/gomod /tmp/gocache; \
        /usr/local/bin/klaudia --version; \
    fi

# The pin above is the contract, so the CLI must not undo it at runtime.
#
# Claude Code checks for a newer release on startup. It cannot replace the
# install it is running — that lives in `/usr/local`, owned by root, and the
# pod runs as `sprooty` — so instead it writes the update to whatever
# `NPM_CONFIG_PREFIX` names, which is the *persisted* `~/.npm-global`. PATH
# puts `/usr/local/bin` first, so the update never takes effect and the CLI
# does it again on the next boot: an unpinned second copy accumulating on the
# data volume, forever, one PATH-ordering change away from winning. That
# ordering has already failed once — a user-managed `~/.npm-global/bin/codex`
# shadowing the system wrapper — and `2.1.236` is pinned precisely because
# `2.1.237` shipped a
# broken `claude.exe`. An updater free to walk past that pin is
# the same exposure with no gate.
#
# `DISABLE_UPDATES` rather than `DISABLE_AUTOUPDATER`: the latter stops the
# background check but leaves `claude update` able to fork the image's version
# anyway. Here the pin *is* the statement of what runs, so every
# vendor update path is closed and a new version arrives one of two ways: by
# editing the manifest above and rebuilding (the baked baseline), or through
# the runtime pin the entrypoint applies — `vogt-agent-cli-install` is
# the only sanctioned writer of a non-image copy.
ENV DISABLE_UPDATES=1
# opencode's own self-update, for the same reason (WI-929): it would otherwise
# walk past the pin from inside a session.
ENV OPENCODE_DISABLE_AUTOUPDATE=1

# Read-only MCP servers an agent may be given (docs/ENGINE.md §4): GitHub,
# Grafana and Gitea/Forgejo. Exact release versions, each tarball checked
# against a sha256 recorded here — not against a digest fetched in the same
# request, which proves only that the download matched what the server said.
# A bump is a deliberate edit of these lines. The pod base also installs a
# `github-mcp-server` resolved to latest; the pinned copy replaces it, so the
# binary a session runs is decided by this file. Nothing is registered with a
# client here: `vogt-mcp-bootstrap` does that per session, and only when the
# session holds the server's read-only token.
ARG GITHUB_MCP_SERVER_VERSION=1.14.0
ARG GITHUB_MCP_SERVER_SHA256_AMD64=2fcad56bb164b6c4918fdd9f1d5572257bf4e1f14b7eb1119d015b079818fd38
ARG GITHUB_MCP_SERVER_SHA256_ARM64=dd06beb81e62c42afa2d4217d86be79ca588a083f63da38ca3cbef0b7785e88d
ARG MCP_GRAFANA_VERSION=2.0.0
ARG MCP_GRAFANA_SHA256_AMD64=0a0dde2c882c24fedcce79a07d97b232f71730c744b84a09b6b0735c2ca3d024
ARG MCP_GRAFANA_SHA256_ARM64=5b29581cad5ce21a0db67c655e5e7e71a9163b4e1ba1b518559baa3c54133a28
ARG GITEA_MCP_VERSION=1.8.0
ARG GITEA_MCP_SHA256_AMD64=0a3aec987216b3299693ecf988f0b80686af6053d093d44148d26502e139f80c
ARG GITEA_MCP_SHA256_ARM64=0714a101ab2163126580078ea1fc2291df306bfbd74e3af1b0f7a09c3f210ebb
RUN set -eu; \
    case "$(dpkg --print-architecture)" in \
        amd64) arch=x86_64; sfx=AMD64 ;; \
        arm64) arch=arm64; sfx=ARM64 ;; \
        *) echo "no pinned MCP servers for $(dpkg --print-architecture)" >&2; exit 1 ;; \
    esac; \
    fetch() { \
        url="$1"; sum="$2"; bin="$3"; \
        curl -fsSL "$url" -o /tmp/mcp.tar.gz; \
        echo "$sum  /tmp/mcp.tar.gz" | sha256sum -c -; \
        mkdir -p /tmp/mcp; tar -xzf /tmp/mcp.tar.gz -C /tmp/mcp "$bin"; \
        install -m 755 "/tmp/mcp/$bin" "/usr/local/bin/$bin"; \
        rm -rf /tmp/mcp /tmp/mcp.tar.gz; \
    }; \
    eval "gh_sum=\$GITHUB_MCP_SERVER_SHA256_$sfx graf_sum=\$MCP_GRAFANA_SHA256_$sfx gitea_sum=\$GITEA_MCP_SHA256_$sfx"; \
    fetch "https://github.com/github/github-mcp-server/releases/download/v${GITHUB_MCP_SERVER_VERSION}/github-mcp-server_Linux_${arch}.tar.gz" "$gh_sum" github-mcp-server; \
    fetch "https://github.com/grafana/mcp-grafana/releases/download/v${MCP_GRAFANA_VERSION}/mcp-grafana_Linux_${arch}.tar.gz" "$graf_sum" mcp-grafana; \
    fetch "https://gitea.com/gitea/gitea-mcp/releases/download/v${GITEA_MCP_VERSION}/gitea-mcp_Linux_${arch}.tar.gz" "$gitea_sum" gitea-mcp; \
    github-mcp-server --version | grep -qF "$GITHUB_MCP_SERVER_VERSION"; \
    mcp-grafana --version | grep -qF "$MCP_GRAFANA_VERSION"; \
    gitea-mcp --version | grep -qF "$GITEA_MCP_VERSION"

# The server binary (built in stage 2 with the web bundle embedded; cache
# mounts in stage 2 mean we have to copy out of /usr/local/bin, not /app).
COPY --from=server-build /usr/local/bin/vogt-engine /usr/local/bin/vogt-engine
RUN chmod +x /usr/local/bin/vogt-engine

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
RUN install -d -o ${SPROOTY_UID} -g ${SPROOTY_GID} -m 0750 /var/lib/vogt \
    && install -d -o ${SPROOTY_UID} -g ${SPROOTY_GID} -m 0755 /opt/vogt/agent-clis

# Runtime scripts. Agent-specific CLIs are optional; these helpers only broker
# credentials on demand for neutral tools already present in the image.
COPY vogt-lifecycle.sh /usr/local/bin/vogt-lifecycle
COPY engine/deploy/entrypoint.sh /usr/local/bin/vogt-entrypoint
COPY engine/deploy/verify-agent-clis.sh /usr/local/bin/vogt-verify-agent-clis
COPY engine/deploy/agent-cli-install.sh /usr/local/bin/vogt-agent-cli-install
COPY engine/deploy/agent-auth.sh /usr/local/bin/vogt-agent-auth
COPY engine/deploy/vogt-mcp-auth.sh /usr/local/bin/vogt-mcp
COPY engine/deploy/mcp-bootstrap.sh /usr/local/bin/vogt-mcp-bootstrap
COPY engine/deploy/git-askpass.sh /usr/local/bin/vogt-git-askpass
COPY engine/deploy/rust-analyzer-mcp.sh /usr/local/bin/vogt-rust-analyzer-mcp
COPY engine/deploy/readonly-mcp.sh /usr/local/bin/vogt-readonly-mcp
COPY engine/deploy/klaudia-mcp.sh /usr/local/bin/vogt-klaudia-mcp
COPY engine/deploy/git-forgejo.sh /usr/local/bin/git-forgejo
COPY engine/deploy/claude-session-hook.sh /usr/local/bin/vogt-claude-session-hook
RUN chmod +x \
    /usr/local/bin/vogt-lifecycle \
    /usr/local/bin/vogt-entrypoint \
    /usr/local/bin/vogt-verify-agent-clis \
    /usr/local/bin/vogt-agent-cli-install \
    /usr/local/bin/vogt-agent-auth \
    /usr/local/bin/vogt-mcp \
    /usr/local/bin/vogt-mcp-bootstrap \
    /usr/local/bin/vogt-git-askpass \
    /usr/local/bin/vogt-rust-analyzer-mcp \
    /usr/local/bin/vogt-readonly-mcp \
    /usr/local/bin/vogt-klaudia-mcp \
    /usr/local/bin/vogt-claude-session-hook \
    /usr/local/bin/git-forgejo

# One port. vogt-core listens on loopback inside this namespace and is not
# exposed here, because there is nothing to expose it to: the engine is the
# only way in.
EXPOSE 8910
# Do NOT declare a VOLUME for /home/sprooty/Working. The deployments mount a
# named volume (`engine-home`) at the *parent* /home/sprooty; an anonymous
# VOLUME at this child path shadows it, landing the work tree — the import root
# and everything sessions edit — on a random anonymous volume that is orphaned
# by `docker compose down` and cannot be shared into a session container by
# name. Left as a normal directory, Working lives inside the named home volume,
# which is what stack.compose.yml already documents. (/var/lib/vogt is fine to
# VOLUME: the compose mounts `vogt-data` at that *exact* path, overriding it.)
VOLUME ["/var/lib/vogt"]

USER sprooty
WORKDIR /home/sprooty/Working

ENV ENGINE_BIND=0.0.0.0:8910
ENV RUST_LOG=info
# An allocation, so it carries a default: it names a
# directory inside this image, which is the one thing the image does know.
ENV VOGT_DATA_DIR=/var/lib/vogt
# `VOGT_CORE_URL` is deliberately *not* defaulted here. It is what decides
# whether this container runs a core at all, and an image that always tries to
# is an image that cannot run as the bare engine on its own — which it must
# stay. The compose file turns the core on; see deploy/entrypoint.sh.

ENTRYPOINT ["/usr/local/bin/vogt-lifecycle"]
CMD ["/usr/local/bin/vogt-entrypoint"]
