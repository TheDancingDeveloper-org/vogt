# `engine/` — Vogt's session engine

The Rust half of Vogt: PTY sessions, WebSocket attach, the SSE event stream,
workspace-scoped file and git APIs, agent tasks, the assistant, and the front
door that publishes the merged product's only port.

**The engine's documentation is [`docs/ENGINE.md`](../docs/ENGINE.md)**, one
level up. That file carries what this README used to: current status, how to
run it, the full wire contract, the assistant's threat model, the agent-task
execution model, the WebSocket protocol and the smoke tests.

This file is a pointer rather than a copy on purpose. It was 384 lines of
MyDevEnv2's own README, kept in a subtree of a product it had been merged into,
with paths that were relative to a tree that no longer existed. A README that
describes a product from inside a directory that is not that product is a
document with no correct set of relative paths, and it drifts in exactly the
places nobody looks.

| Looking for | Read |
|---|---|
| What the engine is, running it, its wire contract, the assistant, agent tasks | [`docs/ENGINE.md`](../docs/ENGINE.md) |
| Rules for agents working in this subtree | [`AGENTS.md`](AGENTS.md), and [`../AGENTS.md`](../AGENTS.md) above it |
| The runtime image's toolchain | [`docs/DEPLOYMENT.md`](../docs/DEPLOYMENT.md) §10 |
| The stacks this subtree deploys to | [`docs/DEPLOYMENT.md`](../docs/DEPLOYMENT.md) §11 |
| Why there are two languages at all | [`docs/MERGE_MYDEVENV2.md`](../docs/MERGE_MYDEVENV2.md) §4 |
| What was designed here and never built | [`docs/REQUIREMENTS.md`](../docs/REQUIREMENTS.md) §7 |

Two things that bite immediately, repeated here because this is where somebody
lands before reading anything else:

- **`engine/` is its own Cargo workspace.** The repository root is not one, so
  `cargo` run from the root finds no manifest. Run it from here.
- **The binary embeds `web/dist/` at compile time.** A `cargo build` without a
  fresh `pnpm build` in the repository-root `web/` ships a stale frontend, which
  is the most common way to fix a UI bug and see nothing change.

The archived GPUI desktop client was not carried across by the merge; it stayed
in the MyDevEnv2 repository, which is now its archive. A reference to `client/`
anywhere in this tree names something that is not here.
