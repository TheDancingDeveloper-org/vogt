# syntax=docker/dockerfile:1
#
# The end-to-end engine image (#295): the locally-built engine plus the
# synthetic agent CLI (#296) and its session preset, BAKED IN rather than
# host-bind-mounted. In CI the runner's filesystem is not the docker daemon's,
# so a `-v ../scripts:/opt/vogt-e2e/scripts` bind does not resolve for the
# daemon — baking is topology-independent. Built on top of `vogt-engine:local`
# (see .github/workflows/e2e.yml, which builds core -> engine -> this).
#
# `fake-agent` shells out to `fake_agent_core.py` beside it (found via
# `dirname $0`), so both land in the same directory; the pod-base the engine is
# built on carries python3. The preset in engine.toml points its command at
# /opt/vogt-e2e/scripts/fake-agent.
ARG BASE_IMAGE=vogt-engine:local
FROM ${BASE_IMAGE}

COPY scripts/fake-agent scripts/fake_agent_core.py /opt/vogt-e2e/scripts/
COPY deploy/e2e.engine.toml /opt/vogt-e2e/engine.toml
