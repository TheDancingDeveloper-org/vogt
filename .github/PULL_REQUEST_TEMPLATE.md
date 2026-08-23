## What

<!-- What does this change, in one or two sentences? -->

## Why

<!-- What problem does it solve, or what does it enable? -->

Closes #

## Verification

Check the gates you ran locally for the surfaces this touches (CI runs on
self-hosted runners and cannot run from a fork as-is — see
`docs/CONTRIBUTING.md`):

- [ ] Python core: `uv run pytest`, `uv run mypy`, `uv run ruff check .`,
      `uv run ruff format --check .`
- [ ] Docs: `uv run python scripts/check_docs.py`
- [ ] PWA (`web/`): `pnpm typecheck`, `pnpm test`, `pnpm test:browser`
      (Playwright)
- [ ] Engine (`engine/`): `cargo fmt --check`, `cargo clippy -- -D warnings`,
      `cargo test --all`
- [ ] Not applicable — this change doesn't touch code these gates cover

## Docs

- [ ] `docs/DESIGN.md`, `docs/ROADMAP.md`, or another doc updated if this
      changes the architecture, product contract, or a documented deferral
- [ ] Not applicable
