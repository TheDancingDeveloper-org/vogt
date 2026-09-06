# A worked custom stack image (#614) — the whole extension contract in a few
# lines. This is the exact shape the private `vogt-dev` / `vogt-prod`
# derivatives take: start FROM the *published* stack digest, add what your
# sessions need, hand the user back, and deploy the result by its own digest.
#
#   docker build -t my-vogt-stack:local deploy/examples/custom-stack
#   # then run it with the overlay beside this file (see README.md)
#
# Nothing here reaches into the maintainer estate: no private secrets broker,
# tailnet, infrastructure MCP, mobile toolchain, private registry, host path,
# or private endpoint. A stranger can build and boot it. That is the point —
# the estate derivatives differ from this only in *which* tools and
# integrations they add, never in the mechanism.

# Pin the digest, not the tag: the whole value of a signed release is that the
# bytes cannot move under you. CI overrides this with the digest of the stack
# it is validating; the default is a real published digest so the file builds
# as-is. Resolve the current one with:
#
#   docker buildx imagetools inspect ghcr.io/thedancingdeveloper-org/vogt-stack:0.5.4
#
ARG STACK_IMAGE=ghcr.io/thedancingdeveloper-org/vogt-stack@sha256:31cda6daa29442d37d128d1bb10959ddf5cc7128a3e443ecde3f6ef1ea0616b0
FROM ${STACK_IMAGE}

# ── image-owned: one tool your sessions need ─────────────────────────────────
# The published image ends as `USER sprooty` (uid 1000). Take root back to
# install, then hand it straight back — an image that forgets the second half
# runs your pod as root, which is a different container from the one whose
# digest you pinned.
USER root
RUN apt-get update \
    && apt-get install --no-install-recommends -y ripgrep \
    && rm -rf /var/lib/apt/lists/*
USER sprooty

# Deployment-owned lifecycle hooks are NOT baked in here — the image's runner
# never copies or discovers scripts from the image. They are mounted read-only
# at /run/vogt/hooks by the overlay beside this file. That split — tools in the
# image, hooks and secrets mounted by the deployment — is the extension
# contract; see docs/CUSTOMISATION.md "Extending the stack image".
