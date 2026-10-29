# syntax=docker/dockerfile:1
#
# The end-to-end engine image: the locally-built engine plus the
# synthetic agent CLI and its session preset, BAKED IN rather than
# host-bind-mounted. In CI the runner's filesystem is not the docker daemon's,
# so a `-v ../scripts:/opt/vogt-e2e/scripts` bind does not resolve for the
# daemon — baking is topology-independent. Built on top of `vogt-engine:local`
# (see .github/workflows/e2e.yml, which builds core -> engine -> this).
#
# `fake-agent` execs `vogt-core fake-agent`; the engine image already carries
# `vogt-core` on PATH, so no Python is needed. The preset in engine.toml points its command at
# /opt/vogt-e2e/scripts/fake-agent.
ARG BASE_IMAGE=vogt-engine:local
FROM ${BASE_IMAGE}

COPY scripts/fake-agent /opt/vogt-e2e/scripts/
COPY deploy/e2e.engine.toml /opt/vogt-e2e/engine.toml
