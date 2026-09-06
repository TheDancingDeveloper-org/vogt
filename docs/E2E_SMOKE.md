# End-to-end stack smoke (#295)

Nothing else exercises the product end to end. The core parity harness is
in-process, `test_front_door.py` pairs two processes, the Playwright suite runs
against a mocked API, and `test_public_delivery.py` only renders
`docker compose config`. This is the mechanical form of the human-on-a-phone
smoke #273 asked for: bring the real two-container stack up from a clean clone
and walk the stranger's path against it.

Two pieces do the work:

- [`scripts/e2e_stack_smoke.sh`](../scripts/e2e_stack_smoke.sh) — the mechanical
  walk. An HTTP client against a base URL and a front-door token; it holds no
  core token and never reaches inside a container, so what it checks is exactly
  what a browser and an agent reach.
- [`.github/workflows/e2e.yml`](../.github/workflows/e2e.yml) — the CI job that
  builds and starts the stack, runs the smoke, runs the live Playwright
  project, and always tears the stack down.

## The one secret: `VOGT_FIXTURE_PAT`

The smoke has two halves, split by a single credential.

| Half | What it needs | What it covers |
| --- | --- | --- |
| Credential-free | nothing | `/health/ready`, the PWA at `/`, the first token, a native work item, a session, a synthetic agent task |
| Forge | `VOGT_FIXTURE_PAT` | reset the fixture, import it, link, sweep, backlog non-empty, PRs observed, a work item that appears upstream |

`VOGT_FIXTURE_PAT` is a GitHub Personal Access Token with **read/write to
`TheDancingDeveloper-org/vogt-fixture`** — classic `repo` scope, plus
`workflow` scope for the fixture's failing-checks pull request file (the same
scope [`docs/FORGE_FIXTURE.md`](./FORGE_FIXTURE.md) describes for
`fixture_reset.py`). Set it as a repository secret named `VOGT_FIXTURE_PAT`.

Unset, the forge half prints `SKIP (no VOGT_FIXTURE_PAT)` and the job still
passes on the credential-free half. So an operator who has not yet minted the
PAT still gets a real end-to-end signal; the forge half turns on the moment the
secret exists.

In CI the PAT is used two ways, and never sent to the stack as a request field:

- written to `deploy/github-token` (git-ignored) and read by the core as its
  `github.com` file token (`VOGT_GITHUB_TOKEN_FILE`, FR-S7), so import, link,
  sweep and the upstream work-create all use it;
- passed to `scripts/fixture_reset.py` (as `VOGT_FIXTURE_TOKEN`) and to the
  GitHub API check that confirms the created work item really landed upstream.

## Running the smoke locally

Against any running stack:

```sh
scripts/e2e_stack_smoke.sh https://vogt.example.com "$FRONT_DOOR_TOKEN"
# or
VOGT_SMOKE_BASE=https://vogt.example.com \
VOGT_SMOKE_TOKEN="$FRONT_DOOR_TOKEN" \
  scripts/e2e_stack_smoke.sh
```

With no token the smoke mints the first one itself through the install
bootstrap (#292), when the instance is still in first-run mode.

To run the forge half too, export the PAT:

```sh
VOGT_FIXTURE_PAT=ghp_xxx \
  scripts/e2e_stack_smoke.sh https://vogt.example.com "$FRONT_DOOR_TOKEN"
```

Each step names what its failure means, and timing is reported per step and as
a total.

### Bringing the stack up the way CI does

This is the two-container *contributor* stack — a core and an engine built
from the checkout — not the published image a deployment runs. The suite
needs it that way: the fake agent below is built into the engine image.

```sh
printf '%s' "$VOGT_FIXTURE_PAT" > deploy/github-token   # empty file ⇒ forge half skips
openssl rand -hex 32 > deploy/vogt-core-token
export VOGT_PUBLIC_URL=http://127.0.0.1:8910
export ENGINE_PUBLIC_URL=http://127.0.0.1:8910
export ENGINE_TOKEN="$(openssl rand -hex 32)"

docker compose \
  -f deploy/vogt.compose.yml \
  -f deploy/engine.overlay.yml \
  -f deploy/e2e.overlay.yml up --build -d

scripts/e2e_stack_smoke.sh http://127.0.0.1:8910 "$ENGINE_TOKEN"

docker compose \
  -f deploy/vogt.compose.yml \
  -f deploy/engine.overlay.yml \
  -f deploy/e2e.overlay.yml down -v
```

The [`deploy/e2e.overlay.yml`](../deploy/e2e.overlay.yml) file adds only what
the smoke needs and the shipped image deliberately does not carry: it mounts
[`scripts/`](../scripts) into the engine and names
[`deploy/e2e.engine.toml`](../deploy/e2e.engine.toml), which registers the
synthetic agent CLI (#296) as the `Fake Agent (test)` session preset — the same
way a deployment registers `claude`/`codex`.

## The live Playwright project

The `live` project in [`web/playwright.config.ts`](../web/playwright.config.ts)
runs the **same** `gui.spec.ts` specs against the running stack — real API, no
`installFixtures` — so a drift between the mocks and the truth fails a test.

It is opt-in: the project exists only when `PLAYWRIGHT_LIVE_BASE_URL` names the
front door, so a bare `playwright test` (the default and every PR) neither
lists nor runs it, and the mocked `desktop`/`phone` projects are untouched. The
CI job selects it explicitly:

```sh
cd web
PLAYWRIGHT_LIVE_BASE_URL=http://127.0.0.1:8910 \
PLAYWRIGHT_LIVE_TOKEN="$FRONT_DOOR_TOKEN" \
  pnpm exec playwright test --project=live
```

`installFixtures` becomes a no-op in this mode (it stops intercepting routes and
seeds the real token in place of the fake one); the specs then exercise the
live front door.

## When it runs

On `dev` merges and nightly, plus `workflow_dispatch` — never on pull requests,
which keep the mocked suites. A docs-only push skips the build. The job is
self-hosted (NFR-C4), on a runner with the `docker` capability.

## The clean-consumer smoke (#613)

`scripts/e2e_stack_smoke.sh` walks a stack somebody else stood up.
`scripts/clean_consumer_smoke.sh` owns the whole consumer path instead: it is
the mechanical form of "a stranger installs the published product", and it is
what proves the AIO is one product stack — the `vogt-stack` pod plus the bundled
`vogt-voice` sidecar — that a third party can run with no checkout build and no
private credential.

```sh
scripts/clean_consumer_smoke.sh
# or pin a specific pair:
VOGT_STACK_IMAGE=ghcr.io/thedancingdeveloper-org/vogt-stack:0.5.4 \
VOGT_VOICE_IMAGE=ghcr.io/thedancingdeveloper-org/vogt-voice:0.5.4 \
  scripts/clean_consumer_smoke.sh
```

It resolves both images' digests over an **anonymous** registry token (the pull
a stranger gets, so a private package fails before anything runs), pulls them by
digest, boots `deploy/stack.compose.yml` from `deploy/stack.env.example` in a
throwaway Compose project, and walks readiness → the PWA → the first token → one
core read/write → a full voice round trip (synthesise a phrase, feed the audio
back, read the transcript — the deterministic STT/TTS routing through the
sidecar). It then repeats with voice disabled and checks that speech reports
unavailable (the engine's 404 fallback) while the rest still works. Each run
writes a receipt (`clean-consumer-receipt.json` by default,
`VOGT_SMOKE_RECEIPT` to redirect) recording each image's digest, labels, and any
SLSA provenance.

Run it on a real consumer host, where the Docker daemon shares the host's
filesystem and loopback. On the containerised CI runners the daemon cannot see a
host-bind secret and the runner cannot reach the stack over host loopback (the
same constraint the `e2e` job documents), so the `clean-consumer` CI job gates
the parts that are robust anywhere — both images resolve over an anonymous token
and the shipped compose renders with voice on and off — and leaves the full
boot-walk to a run on a consumer host.
