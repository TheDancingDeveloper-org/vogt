# gui-egui: the Vogt GUI on egui

A proof of concept of the Vogt GUI as an [egui] app on [fluent2-egui], for
desktop and the browser (WI-1806). It is a standalone Cargo workspace, so the
engine's lockfile and Rust floor are untouched. fluent2-egui needs Rust 1.95.

Slice 1 (WI-2003) covers:

- **Login** with a username and password (`POST /api/auth/login`), or with a
  pasted bearer token. The bearer is held in memory only. Only the API address
  is saved.
- **Sessions**: `GET /api/sessions`, kept live by the `/api/events` SSE stream.
  The stream is read with a streaming fetch, because `EventSource` cannot send
  `Authorization`. A lost stream reconnects with backoff (0.5 s doubling to
  30 s), and the list is re-read on every reconnect and on `lagged`. A 401
  anywhere signs out.
- **Work-item detail**, read-only: `GET /api/vogt/work/get?mode=full`. The body
  and comments render through fluent2's `MarkdownView`. The page re-reads when
  the stream reports a `vogt-changed` for the item.

The Git diff screen (WI-2008) is read-only. The **Changes** button on a
session row reads `GET /api/git/status` for the session's working directory.
The engine takes `repo` relative to its workspace root, which the app reads
once from `GET /api/status`. A directory outside the workspace is reported as
such, not requested. Selecting a file reads `GET /api/git/diff` (the staged
diff for a staged file) and shows it in fluent2's `DiffView`, side by side or
inline.

The remaining screens are child items of WI-1806 (WI-2004 to WI-2015).

## Layout

| module | what |
|---|---|
| `api` | wire types, tolerant of unknown and missing fields |
| `sse` | incremental `text/event-stream` parser |
| `backoff` | reconnect delays |
| `backend` | the `Backend` trait; `HttpBackend` (ehttp, native and wasm) and `FixtureBackend` (in memory) |
| `model` | state and rules with no egui: replies in, commands out |
| `app` | the egui shell on fluent2-egui |

The UI never blocks. It asks the backend for something and reads `Reply`
values back from `Backend::poll` on later frames. Tests drive the model and
the whole app (kittest, no GPU) against `FixtureBackend`.

## Run

```sh
# Desktop. VOGT_BASE presets the API address on first run.
VOGT_BASE=https://vogt-dev.example cargo run -p vogt-gui

# Checks
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test

# Browser build. Serve it from the engine's own origin: the API base defaults
# to the page origin, and the engine sends no CORS headers.
cargo build -p vogt-gui --lib --release --target wasm32-unknown-unknown
wasm-bindgen --target web --out-dir dist target/wasm32-unknown-unknown/release/vogt_gui.wasm
cp vogt-gui/index.html dist/
```

[egui]: https://github.com/emilk/egui
[fluent2-egui]: https://github.com/TheDancingDeveloper-org/fluent2-egui
