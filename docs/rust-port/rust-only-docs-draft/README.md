# Rust-only docs draft (WI-2506)

Edits to apply when the Python core is removed (WI-2505), not before. Until
the swap the image ships both cores and `VOGT_CORE_IMPL` defaults to
`python`, so the current docs are right to mention both. Applying this draft
early would document a deployment that does not exist yet.

Each file here is a drop-in replacement for the docs file of the same name.
A paragraph not mentioned is unchanged. The swap plan that these edits assume
is [`../swap-release-plan.md`](../swap-release-plan.md).

## What changes at the swap

- `VOGT_CORE_IMPL` goes away. There is one core, the Rust binary, and the
  entrypoint always runs it.
- The operator CLI name `vogt` is that binary. `vogt-core` remains as a
  compatibility name for one release, then drops.
- The standalone core image (`ghcr.io/thedancingdeveloper-org/vogt`) becomes
  the Rust core. The estate's split stack
  (`personal/vogt-{dev,prod}/vogt.compose.yml`) pins that image; its command
  stays `vogt init && exec vogt serve`, which is correct once `vogt` is Rust.
- `uv`, `ruff`, `mypy` and `pytest` leave the contributor docs. The core's
  gates become the engine workspace's: `cargo fmt`, `cargo clippy`,
  `cargo test`.

## Files

| Draft | Replaces | What it rewrites |
| --- | --- | --- |
| `DEPLOYMENT.md.md` | `docs/DEPLOYMENT.md` | the topology diagram, the boot sentence, the both-cores section |
| `stack.compose.yml.md` | `deploy/stack.compose.yml` | the `VOGT_CORE_URL` comment and the removal of `VOGT_CORE_IMPL` |
| `ENGINE.md.md` | `docs/ENGINE.md` | the image contents and the entrypoint selection sentence |
