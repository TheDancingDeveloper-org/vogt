# syntax=docker/dockerfile:1
#
# The end-to-end live-Playwright image (#295): the web/ Playwright project and
# its dependencies BAKED IN, so the `live` project runs from INSIDE the compose
# network with no host bind-mount — the CI runner's filesystem is not the docker
# daemon's, the same constraint deploy/e2e.engine.Dockerfile is built for. The
# image tag is version-matched to web/package.json's `@playwright/test`
# (^1.62.1) so the browsers preinstalled in the base match the test runner and
# nothing is downloaded at run time.
#
# The workflow builds this as `vogt-e2e-playwright:local` and runs it as the
# `playwright` service (profile `live`) in deploy/e2e.overlay.yml, pointed at
# the running engine by service DNS (http://engine:8910). It runs ONLY the
# `live` project, which uses the real API (no `installFixtures`) — see
# web/playwright.config.ts and web/tests/browser/gui.spec.ts.
ARG PW_VERSION=v1.62.1-noble
FROM mcr.microsoft.com/playwright:${PW_VERSION}

WORKDIR /web
# corepack ships with the Node base image; it pins pnpm from web/package.json's
# `packageManager` field, matching the lockfile the frozen install asserts.
RUN corepack enable
COPY web/ /web/
RUN pnpm install --frozen-lockfile

# The live project points at the running stack (PLAYWRIGHT_LIVE_BASE_URL, set at
# run time) and does NOT start the Vite dev server. Run only that project.
ENTRYPOINT ["pnpm", "exec", "playwright", "test", "--project=live"]
