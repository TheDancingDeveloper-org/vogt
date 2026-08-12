# syntax=docker/dockerfile:1
#
# The shipped image (NFR-PO4, NFR-D9).
#
# Hardened by construction rather than by compose flags alone: it runs as an
# unprivileged uid that exists in the image, owns nothing it does not need,
# and writes only to the volume and to tmpfs. The compose file adds
# `read_only`, `cap_drop: [ALL]` and `no-new-privileges`; this side makes
# those settings survivable.

FROM python:3.13-slim AS build

# uv resolves and installs from the committed lockfile, so the image contains
# exactly what CI tested (NFR-Q5).
COPY --from=ghcr.io/astral-sh/uv:0.9.18 /uv /usr/local/bin/uv

ENV UV_COMPILE_BYTECODE=1 \
    UV_LINK_MODE=copy \
    UV_PYTHON_DOWNLOADS=never

WORKDIR /src

# Dependencies first, so a source-only change does not re-resolve them.
COPY pyproject.toml uv.lock README.md LICENSE ./
RUN --mount=type=cache,target=/root/.cache/uv \
    uv sync --locked --no-install-project --no-dev

COPY src/ ./src/
RUN --mount=type=cache,target=/root/.cache/uv \
    uv sync --locked --no-dev


FROM python:3.13-slim AS runtime

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

COPY --from=build --chown=root:root /src/.venv /opt/vogt/.venv

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
