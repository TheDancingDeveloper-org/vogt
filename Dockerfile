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
FROM python:3.13-slim@sha256:ffb752e139c0a19692a43af8d8523b274222dd68eebad5d583b45c2201c6e30a AS build

# uv resolves and installs from the committed lockfile, so the image contains
# exactly what CI tested (NFR-Q5).
COPY --from=ghcr.io/astral-sh/uv:0.9.18 /uv /usr/local/bin/uv

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
RUN --mount=type=cache,target=/root/.cache/uv \
    uv sync --locked --no-install-project --no-dev

COPY src/ ./src/
# `--no-editable` because uv.lock records the project as an editable source.
# An editable install is a `.pth` file pointing back at /src, which does not
# exist in the runtime stage — the venv would import every dependency and
# then fail on `No module named 'vogt'`.
RUN --mount=type=cache,target=/root/.cache/uv \
    uv sync --locked --no-dev --no-editable


FROM python:3.13-slim@sha256:ffb752e139c0a19692a43af8d8523b274222dd68eebad5d583b45c2201c6e30a AS runtime

LABEL org.opencontainers.image.title="vogt" \
      org.opencontainers.image.description="A product development environment for the AI era" \
      org.opencontainers.image.source="https://github.com/TheDancingDeveloper-org/vogt" \
      org.opencontainers.image.licenses="MIT"

# A fixed uid so the compose file's `user:` and the bind-mounted token file's
# mode (0750 root:10001) agree with each other (`DEPLOYMENT.md` §2.2).
RUN groupadd --gid 10001 vogt \
    && useradd --uid 10001 --gid 10001 --no-create-home --shell /usr/sbin/nologin vogt \
    && mkdir -p /var/lib/vogt \
    && chown 10001:10001 /var/lib/vogt

COPY --from=build --chown=root:root /opt/vogt/.venv /opt/vogt/.venv

ENV PATH="/opt/vogt/.venv/bin:$PATH" \
    VOGT_DATA_DIR=/var/lib/vogt \
    PYTHONUNBUFFERED=1 \
    PYTHONDONTWRITEBYTECODE=1

VOLUME ["/var/lib/vogt"]
USER 10001:10001

# No default host or port anywhere, including here (NFR-D2): those encode
# exposure, and the compose file is what is allowed to know the answer for a
# particular host. The image therefore has no CMD that would silently bind
# something — `serve` refuses to start without being told where to listen.
ENTRYPOINT ["vogt"]
CMD ["--help"]
