# `engine/core` — the Rust Vogt core

A workspace member of `engine/`. The binary is `vogt-core`. It is being built
as a drop-in replacement for the Python `vogt serve` process: same loopback
port, same HTTP, MCP, and CLI contracts, same SQLite files. The engine, PWA,
and deployed databases do not change. During the overlap the binary stays
named `vogt-core` so it does not shadow `vogt` on a dev pod.

## Contributing

- Work in a dedicated graft clone on line `rust-2`, not on `main`.
  Commit, sync and land with graft; workers do not open pull requests.
  Oversight owns integration with main; do not merge main into the line.
- Acceptance for a chunk is the parity harness:
  `scripts/parity.py check --impl rust` (the harness arrives with the
  parity-framework work; until then, `cargo fmt`, `cargo clippy -D warnings`,
  and `cargo test -p vogt-core` from `engine/` are the gate).
- Do not commit anything under `docs/local/`.
