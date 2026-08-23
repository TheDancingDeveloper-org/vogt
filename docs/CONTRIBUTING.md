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

The public supported path is the Python core and its generic Docker/Compose
delivery (`Dockerfile`, `deploy/vogt.compose.yml`, `deploy/vogt.build.yml`,
`deploy/.env.example`). The Rust session engine, PWA, and mobile shell have
their own toolchains (`engine/AGENTS.md`, [`ENGINE.md`](ENGINE.md)) and their
own image; do not make them a prerequisite for a core change. Read
[`opensource.md`](../opensource.md) before changing packaging or deployment
files — `tests/test_public_delivery.py` pins that the public example stays
self-contained, and a change that introduces a private path, registry, secret
broker, or external service into it will fail there.

Repository-wide conventions for agents and people — layer order, the
transport-parity rule, logging — are in [`AGENTS.md`](../AGENTS.md).

## Pull requests

Describe the behavior change, the surfaces it affects, and the verification
commands run. If a change keeps a compatibility alias for a
historical identifier, explain its migration/removal plan and record it in
[`opensource.md`](../opensource.md) rather than hiding the alias in a generic
example.
