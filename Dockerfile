# syntax=docker/dockerfile:1
#
# The shipped image (NFR-PO4, NFR-D9).
#
# Hardened by construction rather than by compose flags alone: it runs as an
# unprivileged uid that exists in the image, owns nothing it does not need,
# and writes only to the volume and to tmpfs. The compose file adds
# `read_only`, `cap_drop: [ALL]` and `no-new-privileges`; this side makes
# those settings survivable.

# Pinned by digest, not by tag. `DEPLOYMENT.md` §2.2 requires Vogt's own
# published image to be digest-pinned in the ops repo; a floating base tag
# would mean the thing that gets pinned is assembled from something that
# is not. It is also the `update_automation_gap` this product reports on
# other people's repositories (FR-D6) — hard to justify raising for others
# while leaving it here.
#
# This is the multi-arch index digest for `python:3.13-slim`, so it still
# resolves per-platform. Renovate keeps it current (`pinDigests: true`).
#
# This is the upstream Docker Official Image, pinned by digest. Keeping the
# public image on an upstream registry is important: a new operator must be
# able to build Vogt without access to a private organisation mirror. Renovate
# can update this digest; the image digest is deliberately shared by the build
# and runtime stages so the virtualenv is built against the interpreter that
# runs it.
FROM python:3.13-slim@sha256:ffb752e139c0a19692a43af8d8523b274222dd68eebad5d583b45c2201c6e30a AS build

# uv resolves and installs from the committed lockfile, so the image contains
# exactly what CI tested (NFR-Q5).
# Pin the upstream uv image as well as the Python base. Docker accepts a
# tag-plus-digest reference here, so a rebuild cannot silently select a new
# installer binary.
COPY --from=ghcr.io/astral-sh/uv:0.9.18@sha256:5713fa8217f92b80223bc83aac7db36ec80a84437dbc0d04bbc659cae030d8c9 /uv /usr/local/bin/uv

# The venv is built at the path it will be *used* at, not at `/src/.venv`
# and copied. A venv is not relocatable: its console scripts carry an
# absolute shebang, so a venv built at /src/.venv and copied to /opt gives
# `exec /opt/vogt/.venv/bin/vogt: no such file or directory` — an error that
# names the script while actually meaning its interpreter is missing.
ENV UV_COMPILE_BYTECODE=1 \
    UV_LINK_MODE=copy \
    UV_PYTHON_DOWNLOADS=never \
    UV_PROJECT_ENVIRONMENT=/opt/vogt/.venv

WORKDIR /src

# Dependencies first, so a source-only change does not re-resolve them.
COPY pyproject.toml uv.lock README.md LICENSE ./
RUN uv sync --locked --no-install-project --no-dev

COPY src/ ./src/
# `--no-editable` because uv.lock records the project as an editable source.
# An editable install is a `.pth` file pointing back at /src, which does not
# exist in the runtime stage — the venv would import every dependency and
# then fail on `No module named 'vogt'`.
RUN uv sync --locked --no-dev --no-editable


FROM python:3.13-slim@sha256:ffb752e139c0a19692a43af8d8523b274222dd68eebad5d583b45c2201c6e30a AS runtime

LABEL org.opencontainers.image.title="vogt" \
      org.opencontainers.image.description="A product development environment for the AI era" \
      org.opencontainers.image.source="https://github.com/TheDancingDeveloper-org/vogt" \
      org.opencontainers.image.licenses="MIT"

# Runnable as **any** uid, chosen at deploy time. Which uid is right is a
# property of the host — of who owns the files being observed — so it is a
# compose concern, and needing a release to change it would be a defect.
#
# The obstacle is Docker, not policy: a fresh named volume is seeded from the
# ownership of this directory in the image, so a directory owned by a
# specific uid silently breaks for every deployer who is not that uid — and
# breaks on volume *recreation*, typically during a restore.
#
# Solved the usual way: the data directory is owned by group 0 and is
# group-writable, so any uid running with gid 0 can write to it. Deployers
# set `user: "<their-uid>:0"` and rebuild nothing. `USER 1000:0` below is a
# default, not a requirement.
#
# A passwd entry at the default uid keeps `local:<os-user>` readable for the
# common case; at any other uid the principal falls back to `$USER`, which
# the compose file sets. Neither is load-bearing — provenance for anything
# over the network comes from the token, never from the OS user (FR-S2).
# `git` is a runtime dependency, not a build one. `project.import` shells out
# to it to clone and to recognise an existing checkout (FR-P6, FR-P7), and the
# `git-local` collector shells out to it to read branch, head and dirty state.
# The first release of this image shipped without it, which left import unable
# to run at all and the collector recording an observation it had never read
# (#19, #20, #21) — so it is installed here, and `tests/test_deploy.py` asks
# the built image for it rather than trusting this line.
RUN apt-get update \
    && apt-get install -y --no-install-recommends git \
    && rm -rf /var/lib/apt/lists/*

RUN groupadd --gid 1000 vogt \
    && useradd --uid 1000 --gid 0 --no-create-home --shell /usr/sbin/nologin vogt \
    && mkdir -p /var/lib/vogt \
    && chown root:0 /var/lib/vogt \
    && chmod 0770 /var/lib/vogt

COPY --from=build --chown=root:root /opt/vogt/.venv /opt/vogt/.venv

ENV PATH="/opt/vogt/.venv/bin:$PATH" \
    VOGT_DATA_DIR=/var/lib/vogt \
    PYTHONUNBUFFERED=1 \
    PYTHONDONTWRITEBYTECODE=1

VOLUME ["/var/lib/vogt"]
USER 1000:0

# No default host or port anywhere, including here (NFR-D2): those encode
# exposure, and the compose file is what is allowed to know the answer for a
# particular host. The image therefore has no CMD that would silently bind
# something — `serve` refuses to start without being told where to listen.
ENTRYPOINT ["vogt"]
CMD ["--help"]
