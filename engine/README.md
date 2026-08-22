# `engine/` — Vogt's session engine

The Rust half of Vogt: PTY sessions, WebSocket attach, the SSE event stream,
workspace-scoped file and git APIs, agent tasks, the assistant, and the front
door that publishes the merged product's only port. It is an **optional**
component: the Python core at the repository root is a complete product on its
own, and this subtree is built from source with `engine/Dockerfile`.

**The engine's documentation is [`docs/ENGINE.md`](../docs/ENGINE.md)**, one
level up. That file carries what this README used to: current status, how to
build and run it, the full wire contract, the assistant's configuration and
threat model, the agent-task execution model, the WebSocket protocol and the
smoke tests.

This file is a pointer rather than a copy on purpose. It was several hundred
lines of the engine's pre-merge README, kept in a subtree of a product it had
been merged into, with paths that were relative to a tree that no longer
existed. A README that describes a product from inside a directory that is not
that product is a document with no correct set of relative paths, and it
drifts in exactly the places nobody looks.

| Looking for | Read |
|---|---|
| What the engine is, building and running it, its wire contract, the assistant, agent tasks | [`docs/ENGINE.md`](../docs/ENGINE.md) |
| Rules for agents working in this subtree | [`AGENTS.md`](AGENTS.md), and [`../AGENTS.md`](../AGENTS.md) above it |
| Deploying the images, including the optional engine | [`docs/DEPLOYMENT.md`](../docs/DEPLOYMENT.md) |
| Why there are two languages at all | [`docs/DESIGN.md`](../docs/DESIGN.md) |
| The voice assistant's delivery status | [`docs/VOICE_DELIVERY.md`](../docs/VOICE_DELIVERY.md) |

Two things that bite immediately, repeated here because this is where somebody
lands before reading anything else:

- **`engine/` is its own Cargo workspace.** The repository root is not one, so
  `cargo` run from the root finds no manifest. Run it from here.
- **The binary embeds `web/dist/` at compile time.** A `cargo build` without a
  fresh `pnpm build` in the repository-root `web/` ships a stale frontend, which
  is the most common way to fix a UI bug and see nothing change.

The native desktop client the engine once had was not carried across by the
merge. A reference to `client/` anywhere in this tree names something that is
not here.
