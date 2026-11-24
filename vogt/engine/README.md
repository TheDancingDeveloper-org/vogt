# `engine/` — Vogt's session engine

The Rust half of Vogt: PTY sessions, WebSocket attach, the SSE event stream,
workspace-scoped file and git APIs, agent tasks, the assistant, and the front
door that publishes the stack's only port.

Its documentation is [`docs/ENGINE.md`](../docs/ENGINE.md); rules for working in
this subtree are in [`AGENTS.md`](AGENTS.md).

## Project-keyed session images

`engine/Dockerfile.pod` defines three additive session layers: `base` for
Python/Node coding sessions, `rust` for Rust projects, and `mobile` for Flutter
and Android projects. `.github/session-images-bake.hcl` exposes all three
BuildKit targets without publishing them. Estate CI supplies the version tag
and registry output; no credentials or image-push workflow is stored here.
Project records select the layer (`vogt` uses `rust`, FarmEggs uses `mobile`,
and Python-only projects use `base`).
