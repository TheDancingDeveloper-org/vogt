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

## Before opening a change

- Add or update tests, including a parity case in `tests/test_parity.py` for
  a new operation.
- Keep writes behind the audited write path and require a principal and
  reason.
- Preserve the declared/observed split: collectors return findings and do
  not write authoritative state.
- Keep optional integrations optional. A missing GitHub token, MCP client,
  Cadastre deployment, or session engine must not break core workflows.
- Update the relevant design or requirements revision when a decision changes
  the architecture or the product contract.
- Run the generated-document and link checks before submitting.

## Scope boundaries

The public supported path is the Python core and its generic Docker/Compose
delivery. The Rust session engine and mobile shell have their own toolchains
and deployment concerns; do not make them a prerequisite for a core change.
Read [`opensource.md`](../opensource.md) before changing packaging or
deployment files.

## Pull requests

Describe the behavior change, the surfaces it affects, and the verification
commands run. If a change keeps a compatibility alias for a private or
historical identifier, explain its migration/removal plan rather than hiding
the alias in a generic example.
