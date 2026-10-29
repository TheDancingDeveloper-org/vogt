# `engine/core` — the Rust Vogt core

A workspace member of `engine/`. The binary is `vogt-core`. It is being built
as a drop-in replacement for the Python `vogt serve` process: same loopback
port, same HTTP, MCP, and CLI contracts, same SQLite files. The engine, PWA,
and deployed databases do not change. During the overlap the binary stays
named `vogt-core` so it does not shadow `vogt` on a dev pod.

## Contributing

- Pull requests target the long-lived branch `rust-core`, not `main`.
  Check the branch out as its own worktree; do not do port work on `main`.
- Rebase `rust-core` onto `origin/main` about weekly. The crate is additive,
  so conflicts should be limited to the workspace member list and CI.
- Acceptance for a chunk is the parity harness:
  `scripts/parity.py check --impl rust` (the harness arrives with the
  parity-framework work; until then, `cargo fmt`, `cargo clippy -D warnings`,
  and `cargo test -p vogt-core` from `engine/` are the gate).
- Do not commit anything under `docs/local/`.
