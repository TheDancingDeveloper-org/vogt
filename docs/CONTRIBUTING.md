# Contributing to Vogt

Vogt ships as one product stack: the `vogt-stack` image (the Python core, the
Rust session engine and the Solid PWA behind one port) plus the optional
`vogt-voice` sidecar. As a contributor you do not run that image. You run the
pieces you are changing — a core built from your checkout, an engine built
beside it — and the gates that CI runs. This document is the contributor's
path: what to agree to, how to work, how to check, and how to run what you
changed.

Architecture and conventions live in [`../AGENTS.md`](../AGENTS.md): layer
order, the transport-parity rule, the audited write path, logging. Read it
before your first change. This file does not repeat it.

## 1. Before you start

**Licence.** Vogt is licensed under the GNU Affero General Public License,
version 3 only ([`../LICENSE`](../LICENSE)). Contributions are accepted under
the same licence — inbound equals outbound. There is no contributor licence
agreement and no copyright assignment: you keep your copyright and license your
change to the project under AGPL-3.0-only, exactly as the project licenses it
to everyone else.

**Developer Certificate of Origin.** Every commit carries a sign-off:

```console
git commit -s
```

That adds a `Signed-off-by: Your Name <you@example.com>` trailer. By adding it
you certify the [Developer Certificate of Origin](https://developercertificate.org/):
that you wrote the change or have the right to submit it, that you submit it
under the project's licence, and that you understand the contribution and its
sign-off are public and kept with the project. Use a real name and an address
you control. A pull request whose commits lack the trailer is not merged;
`git commit --amend -s` and `git rebase --signoff` add it after the fact.

**AI-assisted work** is welcome and expected — see
[`../AI_POLICY.md`](../AI_POLICY.md). The sign-off is yours, not the agent's:
you are accountable for what you submit and must be able to explain it.

**Conduct.** The [code of conduct](../CODE_OF_CONDUCT.md) applies to every
project space. Security reports go to the channel in
[`../SECURITY.md`](../SECURITY.md), never to a public issue.

## 2. Workflow

- `main` is the only long-lived branch. Branch from `main`, open the pull
  request against `main`. Releases are `v*` tags on `main`.
- Keep pull requests small and single-purpose. A nearby cleanup belongs in the
  same change only when the change is not correct without it.
- Keep the branch rebased onto `origin/main`. History is linear, so a stale
  base is the usual reason a green PR cannot land.
- Fill in the [pull request template](../.github/PULL_REQUEST_TEMPLATE.md):
  what changed, why, which gates you ran, which documents you updated.
- Documentation changes in the same pull request as the code it describes:
  [`ARCHITECTURE.md`](ARCHITECTURE.md) for an architectural or product
  decision, [`ENGINE.md`](ENGINE.md) for the wire contract, the user and
  deployment guides for externally visible behaviour, and the generated
  configuration reference (§3) whenever `src/vogt/config.py` changes.
- Say why in words. Vogt has no requirement-numbering scheme, and a bare
  ticket number is not a reason: the commit message and the PR description
  state the problem and the behaviour change in prose, so a reader without
  access to any tracker can follow the decision.
- Do not add compatibility aliases for identifiers a change renames. No
  released installation depends on the older names in the tree; remove them.

## 3. Local setup and gates

The core is a plain Python package managed with [uv](https://docs.astral.sh/uv/):

```console
uv sync --locked
uv run vogt init
```

[`scripts/check.sh`](../scripts/check.sh) runs what CI runs, per half, and is
the local floor for every pull request:

```console
scripts/check.sh                    # python + engine + web
scripts/check.sh python web engine  # one or more named halves
```

| Half | What it runs |
| --- | --- |
| `python` | `ruff check .`, `ruff format --check .`, `mypy` (strict), `scripts/check_docs.py`, `pytest` |
| `web` | `pnpm typecheck`, `pnpm test` (Vitest) in `web/` |
| `engine` | `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` in `engine/` |

A half whose tree is absent from the checkout is skipped and says so; a
core-only checkout is a supported shape. `pytest` includes the
transport-parity matrix and a coverage floor of 80% (`--cov-fail-under=80` in
`pyproject.toml`). CI runs the Python half on the declared minimum (3.11) and
a current release (3.13).

The script does not run everything. Run these yourself when they apply:

- **Browser tests.** `cd web && pnpm test:browser` when routing, layout,
  responsive behaviour or screenshots changed. Refresh a snapshot only after
  inspecting the rendered result at desktop and phone sizes.
- **Generated configuration docs.** `docs/CONFIG.md` and
  `config.example.toml` are generated from `src/vogt/config.py`; never edit
  them by hand. After a schema change:

  ```console
  uv run python scripts/gen_config_docs.py
  git diff --exit-code -- docs/CONFIG.md config.example.toml
  ```

  [`scripts/check_docs.py`](../scripts/check_docs.py) (in the `python` half)
  checks that every relative link and heading anchor in the Markdown resolves;
  it does not fetch external URLs.
- **Product version.** `uv run python scripts/check_product_version.py`
  confirms `pyproject.toml`, `src/vogt/__init__.py`, `web/package.json`,
  `mobile/package.json` and the workflows agree on one version; a release bump
  touches all of them. CI passes the expected version as an argument.
- **Voice sidecar.** `voice/` is its own Cargo workspace with native
  prerequisites (`voice/README.md`): `cargo fmt --all -- --check`,
  `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings`,
  `cargo test --locked --workspace`.
- **Android shell.** A PWA change needs no APK. For native changes under
  `mobile/`, run `npx cap sync android` and `./gradlew test assembleDebug`
  with the explicit non-production application id and a deliberate server
  URL, as [`ci.yml`](../.github/workflows/ci.yml) does.

Manifests and lockfiles travel together: let `uv`, Cargo or `pnpm` update the
lockfile and commit both.

## 4. The two-container contributor stack

Deploying Vogt means running the published `vogt-stack` image with
[`deploy/stack.compose.yml`](../deploy/stack.compose.yml)
([`GETTING_STARTED.md`](GETTING_STARTED.md)). Contributing means running the
code in your working tree, and for that there is a second shape that exists
only here: the **contributor stack**, two containers built from the checkout.

| File | Adds |
| --- | --- |
| [`deploy/vogt.compose.yml`](../deploy/vogt.compose.yml) | the base: the core alone, on port 8080 |
| [`deploy/vogt.build.yml`](../deploy/vogt.build.yml) | builds the core from the root `Dockerfile` instead of pulling it |
| [`deploy/engine.overlay.yml`](../deploy/engine.overlay.yml) | builds the engine from `engine/Dockerfile` and puts it in front, on port 8910 |

Each overlay states only its difference from the base. To run a core you
built with an engine you built:

```console
cp deploy/.env.example deploy/.env          # set ENGINE_TOKEN (16+ characters)
openssl rand -hex 32 > deploy/vogt-core-token
docker build -t vogt-core:local -f Dockerfile .
VOGT_IMAGE=vogt-core:local docker compose \
  -f deploy/vogt.compose.yml -f deploy/engine.overlay.yml up --build -d
```

`VOGT_IMAGE` names both the core the base runs and the `CORE_IMAGE` the engine
build lifts into its own image, so one variable points both at your build. The
engine is then the front door at `http://127.0.0.1:8910`: the PWA at `/`,
`/api/vogt/*` proxied to the core, `/mcp` for agents, `ENGINE_TOKEN` as the
bearer. Both containers read the core token from `deploy/vogt-core-token`, so
the engine reaches the core from the first start. `down -v` discards the
volumes for a clean instance. Exposure stays on loopback unless you set
`ENGINE_BIND`; [`DEPLOYMENT.md`](DEPLOYMENT.md) explains the variables.

Which shape to use:

- **A core-only change** (registry, services, storage, CLI, REST, MCP) needs no
  container: `uv sync && uv run vogt init` and the `python` gate. The core
  never depends on the engine, and CI proves it by deleting `engine/`, `web/`
  and `mobile/` before running the suite.
- **A change you want to see through the PWA**, or any engine or PWA change:
  the contributor stack above.
- **The product a user runs**: the published stack. Use it to reproduce a
  report against a release, never to test a change — it does not contain your
  code.

[`tests/test_public_delivery.py`](../tests/test_public_delivery.py) renders
every public Compose file and fails on any private registry, host path, secret
broker or external service: the contributor stack has to stay something a
stranger can bring up from a clone.

## 5. The end-to-end stack smoke

Every other test stops short of the whole product: the parity harness is
in-process, `tests/test_front_door.py` pairs two processes, the Playwright
suite runs against a mocked API, `test_public_delivery.py` only renders
Compose. [`scripts/e2e_stack_smoke.sh`](../scripts/e2e_stack_smoke.sh) walks
the real stack the way a stranger does — an HTTP client against a base URL and
a front-door token, holding no core token and never reaching inside a
container, so it exercises exactly what a browser or an agent reaches. Each
step names what its failure means and reports its timing.

It has two halves, split by one credential:

| Half | Needs | Proves |
| --- | --- | --- |
| Credential-free | nothing | `/healthz`, the PWA at `/`, the first token (minted through the install bootstrap when the instance is still in first-run mode), a native work item, a session, a synthetic agent task through the `fake-agent` preset |
| Forge | a write token for the public fixture repository (§6) | reset the fixture, import it, link it upstream, sweep, backlog non-empty, pull requests observed, a work item that appears upstream (then closed as cleanup) |

Without the token the forge half prints `SKIP` and the run passes on the half
it could execute. Run it against any stack:

```console
scripts/e2e_stack_smoke.sh https://vogt.example.com "$FRONT_DOOR_TOKEN"
# or, keeping the token out of `ps`:
VOGT_SMOKE_BASE=https://vogt.example.com VOGT_SMOKE_TOKEN="$FRONT_DOOR_TOKEN" \
  scripts/e2e_stack_smoke.sh
# with the forge half:
VOGT_FIXTURE_TOKEN=github_pat_xxx scripts/e2e_stack_smoke.sh http://127.0.0.1:8910 "$ENGINE_TOKEN"
```

The agent step needs the synthetic agent registered as a session preset, which
the shipped image deliberately does not carry.
[`deploy/e2e.overlay.yml`](../deploy/e2e.overlay.yml) layers that onto the
contributor stack: a dedicated engine image
([`deploy/e2e.engine.Dockerfile`](../deploy/e2e.engine.Dockerfile)) bakes in
`scripts/fake-agent` and [`deploy/e2e.engine.toml`](../deploy/e2e.engine.toml),
and a one-shot `token-init` service writes the run's tokens into a named
volume so nothing depends on a host path. The build and `up` steps in
[`e2e.yml`](../.github/workflows/e2e.yml) bring that stack up locally
unchanged; they need Docker and nothing else.

**The live Playwright project.** The `live` project in
[`web/playwright.config.ts`](../web/playwright.config.ts) runs the same
`gui.spec.ts` specs against the running stack — real API, no route mocks — so
drift between the mocks and the truth fails a test. It registers only when
`PLAYWRIGHT_LIVE_BASE_URL` is set, so a bare `playwright test` never lists it:

```console
cd web
PLAYWRIGHT_LIVE_BASE_URL=http://127.0.0.1:8910 PLAYWRIGHT_LIVE_TOKEN="$ENGINE_TOKEN" \
  pnpm exec playwright test --project=live
```

**What CI's `e2e` job does.** The workflow runs on pushes to `main` (skipping
docs-only pushes), nightly, and on manual dispatch — never on pull requests,
which keep the mocked suites. On a self-hosted runner with the `docker` label
it builds the local images, starts the three-file stack, runs the smoke and
the live Playwright project from inside the Compose network (the runner is
itself a container and cannot reach the stack over host loopback), prints the
stack logs on failure, and always tears down with `down -v`. The forge half
uses the repository secret `VOGT_FIXTURE_TOKEN` (§6); without it the job still
passes on the credential-free half. Two sibling jobs check the published
product: `vogt-stack` and `vogt-voice` resolve over an anonymous registry
token and the shipped Compose renders with voice on and off, and the
custom-image example under `deploy/examples/` builds from the published stack.

**The clean-consumer smoke.** [`scripts/clean_consumer_smoke.sh`](../scripts/clean_consumer_smoke.sh)
owns the whole consumer path rather than walking a stack someone else started:
it resolves both published images anonymously, pulls them by digest, boots
`deploy/stack.compose.yml` from `deploy/stack.env.example` in a throwaway
project, walks readiness, the PWA, the first token, one core write and a
speech round trip through the sidecar, repeats with voice disabled to confirm
speech reports unavailable while the rest works, and writes a receipt of
digests and provenance. Run it on a real host whose Docker daemon shares the
host's filesystem and loopback; the containerised runners cannot, which is why
CI gates only the resolve-and-render parts.

## 6. The public forge fixture

The forge adapter is normally tested against a fake HTTP transport
([`tests/test_forge_provider.py`](../tests/test_forge_provider.py)): fast and
deterministic, but it never meets real pagination, token scopes, check runs or
the write-back verbs. `TheDancingDeveloper-org/vogt-fixture` closes that gap:
a small, public, real GitHub repository driven to a known state that an opt-in
suite and the e2e smoke read and write.

| Piece | Path |
| --- | --- |
| The known state, as data | [`tests/fixtures/forge_fixture_manifest.json`](../tests/fixtures/forge_fixture_manifest.json) |
| The script that builds it | [`scripts/fixture_reset.py`](../scripts/fixture_reset.py) |
| The opt-in suite | [`tests/test_forge_live.py`](../tests/test_forge_live.py) |

The manifest is the single source of truth: the script builds to it and the
tests grade against its `expected` counts. It holds five labels (`bug`,
`enhancement`, `documentation`, `good first issue`, `wontfix`); two open
milestones; five issues, one of each kind (open bug, open enhancement, closed
bug, open documentation, closed `wontfix`); four pull requests (one open, one
draft, one merged whose body closes an issue, one whose CI workflow is expected
to fail); four branches; and files a posture read can find — a module with
`TODO`/`FIXME` markers, a `requirements.txt`, a `.github/dependabot.yml`.

The branches follow Vogt's branch-binding convention — a branch belongs to a
work item when its name carries the item's forge number — in its three shapes,
`gh-<n>-<slug>`, `feature/gh-<n>-<slug>` and `wi-<n>/<slug>`, and double as
the pull requests' head branches. GitHub numbers issues and pull requests from
one counter, so on a fresh repository built in manifest order the issues take
the first five numbers and the pull requests the next four; the script never
renumbers after that.

**Resetting it.** The script is idempotent and never destroys history: it
finds each object by a stable key (label name, issue title, head branch),
creates it only when absent, then relabels and closes or reopens to match. It
issues no delete and never force-pushes, so a re-run reconciles drift.

```console
uv run python scripts/fixture_reset.py --dry-run             # the plan, no network
uv run python scripts/fixture_reset.py --token-file ./token  # or --token, $VOGT_FIXTURE_TOKEN, $GH_TOKEN
```

**Running the live suite.** The tests carry the `live_forge` marker and are
skipped by default: `tests/conftest.py` skips every marked item unless the run
selected them with `-m live_forge`, and the tests skip themselves when no token
is present. The manifest-consistency tests in the same file carry no marker and
run in the ordinary suite, so the manifest stays coherent without a network.

```console
uv run pytest                                          # live tests collected, skipped
VOGT_FIXTURE_TOKEN=github_pat_xxx uv run pytest -m live_forge
```

`--repo owner/name` on the script and `VOGT_FIXTURE_REPO` for the tests and
the smoke point at another repository; both default to the manifest's `repo`.

**Tokens.** Reading the fixture needs no credential — it is public. Writing to
it (the reset, the live write-back probe, the smoke's forge half) needs a
fine-grained personal access token scoped to that one repository with write
access to contents, issues, pull requests and workflows (the last for the
failing-checks pull request's workflow file). CI holds one as the repository
secret `VOGT_FIXTURE_TOKEN`, and it can reach nothing else. `fixture_reset.py`,
the live suite and `e2e_stack_smoke.sh` all read `VOGT_FIXTURE_TOKEN` (the
first two also accept `GH_TOKEN`). A fork does not carry
the secret; its e2e run skips the forge half, and the jobs that need the token
are not required checks. To exercise the write path yourself, create your own
fixture repository with a README commit, run the reset against it with
`--repo`, and point the tests and smoke at it with `VOGT_FIXTURE_REPO`.

## 7. CI

Every workflow job runs on the maintainer's self-hosted runners, selected by
static labels (`self-hosted`, plus a capability such as `docker`). No job names
a hosted runner and none selects one dynamically.
[`runner-policy.yml`](../.github/workflows/runner-policy.yml) is the gate for
that: it fails any job that does not name a self-hosted runner or that uses an
Actions expression in `runs-on`, and any third-party action not pinned to a
full commit SHA. A queued job is a runner-capacity question; changing
`runs-on` is not the fix and fails the gate.

[`ci.yml`](../.github/workflows/ci.yml) runs on every pull request and on
pushes to `main`. A `changes` job classifies the diff and runs only the halves
it touches; a change to anything shared (workflows, the Dockerfile, `deploy/`,
`.gitignore`) or any push runs everything. The halves are `build` (the Python
gates on 3.11 and 3.13, with the generated-config drift check and the
product-version check), `core` (the Python suite again with `engine/`, `web/`
and `mobile/` deleted first), `engine` (Rust fmt, `cargo audit`, clippy and
tests, with the PWA type-checked, tested, its demo Playwright project run,
built and embedded first, plus the first-screen bundle budget), `voice`,
`android` (a debug APK against the sanitised Firebase placeholder) and
`security` (no live Firebase configuration or API key is tracked).

The `ci` job at the end is the single gate: it succeeds trivially on a
docs-only diff and fails whenever any half failed. **Required status checks on
`main` are `ci` and `runner-policy`.** The `e2e` workflow (§5) is not
required; it runs after a merge and nightly.

**Pull requests from forks.** A fork has no runner registered to it, so the
workflows do not run on the fork's own Actions. A pull request from a fork
runs against this repository's runners only after a maintainer approves the
workflow run, on every push to the PR until it is merged; that approval is the
point at which submitted code is allowed onto the self-hosted pool. Repository
secrets are not available to fork pull requests, so the jobs that need them
skip. None of this is a barrier to contributing: the gates in §3 are what CI
runs, and a PR whose description records them passing locally is reviewed on
that basis while the approved run confirms it.

## 8. Third-party licences

Every dependency Vogt ships or builds with is under a licence compatible with
AGPL-3.0-only; none is under a GPL-family or otherwise incompatible licence.
The only AGPL-licensed packages in any dependency graph are Vogt's own
(`vogt`, `vogt-engine-*`, `vogt-voice-*`). Rerun the audit whenever a manifest
changes and record any new licence family in the pull request:

```console
cd engine && cargo tree --format "{p} {l}" --prefix none | sort -u
cd voice  && cargo tree --format "{p} {l}" --prefix none | sort -u
uvx pip-licenses --python .venv/bin/python --format=plain --order=license
cd web    && pnpm licenses list            # add --prod for what ships
cd mobile && pnpm licenses list
```

Licence families found, by half:

| Half | Families |
| --- | --- |
| Python (`uv.lock`, runtime and dev) | MIT, MIT-0, BSD-2-Clause, BSD-3-Clause, Apache-2.0, PSF-2.0, MPL-2.0 (`certifi`, `pathspec`) |
| Engine (`engine/Cargo.lock`) | MIT, Apache-2.0 (some `WITH LLVM-exception`), BSD-2-Clause, BSD-3-Clause, ISC, Zlib, 0BSD, Unlicense, Unicode-3.0, CC0-1.0, BSL-1.0 (offered as an alternative to Apache-2.0), MPL-2.0 (`ece`), CDLA-Permissive-2.0 (`webpki-roots`, root-certificate data) |
| Voice (`voice/Cargo.lock`) | as the engine, plus MPL-2.0 for the `symphonia` audio crates |
| PWA (`web/pnpm-lock.yaml`) | production: MIT, 0BSD, `dompurify` under MPL-2.0 OR Apache-2.0; development only: Apache-2.0, ISC, BSD, MPL-2.0 (`lightningcss`), BlueOak-1.0.0, CC0-1.0, CC-BY-4.0 (`caniuse-lite` browser data) |
| Android shell (`mobile/pnpm-lock.yaml`) | MIT, ISC, Apache-2.0, BlueOak-1.0.0, Unlicense, 0BSD |

MPL-2.0 is a file-level copyleft that permits combination with AGPL code;
CDLA-Permissive-2.0, BlueOak-1.0.0 and CC-BY-4.0 are permissive and attach to
data or build-time tooling rather than to code that ships. A new dependency
under GPL-2.0-only, SSPL, a Commons Clause or a non-commercial licence is not
acceptable; ask before adding one under any licence not listed above.
