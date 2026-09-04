# Contributing to Vogt

Vogt keeps the domain in Python and treats the operation registry as the
authority for every supported surface. A change to a capability normally
belongs in `src/vogt/application/services/`, its models in
`src/vogt/application/models.py`, and one registry entry in
`src/vogt/registry/operations.py`. The CLI, REST, and MCP adapters are thin;
they should not make independent product decisions.

## Set up

```console
uv sync
uv run pytest
uv run mypy
uv run ruff check .
uv run ruff format --check .
uv run python scripts/check_docs.py
```

If configuration fields change, regenerate the committed reference files:

```console
uv run python scripts/gen_config_docs.py
```

A change under `engine/` or `web/` has its own toolchain and gates
(`engine/AGENTS.md`, [`ENGINE.md`](ENGINE.md)); run them from their own
directories, not the repository root:

```console
cd engine && cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
cd web && pnpm install && pnpm typecheck && pnpm test && pnpm test:browser
```

## CI runs on self-hosted runners — a fork cannot run it as-is

`.github/workflows/runner-policy.yml` fails any job that names a hosted
runner (`ubuntu-latest` and friends); every job in this repository's
workflows names a self-hosted one instead, because the runners have access
this project's maintainer does not want to hand a hosted worker (see the
comment at the top of that workflow). That is a deliberate, durable choice,
not an oversight — do not "fix" a queued or failing check by changing
`runs-on`.

The practical effect: if you fork this repository, GitHub Actions will not
run your fork's copy of these workflows (there is no runner registered to
your fork), and a pull request from a fork runs against *this* repository's
runners only after a maintainer approves it. Either way, run the commands
above locally (or the ones in `AGENTS.md`) before opening a PR — they are
exactly what CI runs — and use the PR template's checklist to record what you
ran. This split between fork-safe local checks and self-hosted CI is tracked
as its own piece of work (#207); it is not solved by this file.

## Before opening a change

- Add or update tests, including a parity case in `tests/test_parity.py` for
  a new operation.
- Keep writes behind the audited write path and require a principal and
  reason.
- Preserve the declared/observed split: collectors return findings and do
  not write authoritative state.
- Keep optional integrations optional. A missing GitHub token, MCP client,
  external MCP server, assistant provider, or session engine must not break
  core workflows; it must surface as "not collected" or "not configured".
- Update `docs/DESIGN.md` when a decision changes the architecture or the
  product contract, and `docs/ROADMAP.md` when something designed is
  deferred.
- Run the generated-document and link checks before submitting.

## Scope boundaries

The supported public product is one image: the Python core, Rust session
engine, and Solid PWA together (`DEPLOYMENT.md` §1.1). The core stays
independently *runnable* — over CLI, REST and MCP from a plain `uv sync`, and
CI keeps it free of engine dependencies — so a core-only change need not
touch the engine's toolchain (`engine/AGENTS.md`, [`ENGINE.md`](ENGINE.md))
unless it changes a contract. To run what you are changing, use the
two-container contributor stack: `deploy/vogt.compose.yml` with
`deploy/vogt.build.yml` for a core built here, plus `deploy/engine.overlay.yml`
for an engine built here (`DEPLOYMENT.md` §3). Before changing packaging or
deployment files, know that `tests/test_public_delivery.py` pins the public
files as self-contained: a change that introduces a private path, registry,
secret broker, or external service into them will fail there.

Vogt is currently a single-maintainer project. Protected branches therefore
require the test and policy checks, resolved review conversations, linear
history, and administrator enforcement, but not a second approving review:
requiring one would make the only maintainer unable to land changes and GitHub
does not let an author self-approve. Outside contributions still receive
maintainer review through the normal pull-request path. Revisit this explicit
tradeoff when a second regular maintainer can share approval duty.

Repository-wide conventions for agents and people — layer order, the
transport-parity rule, logging — are in [`AGENTS.md`](../AGENTS.md).

## Pull requests

Describe the behavior change, the surfaces it affects, and the verification
commands run. Do not add compatibility aliases for historical identifiers:
no released installation depends on the legacy names still in the tree, and
removing them outright is preferred over extending them (see
[`ROADMAP.md`](ROADMAP.md) "Pending cleanup").
