# MyDevEnv2 Desktop Client

A native, high-performance desktop client for the MyDevEnv2 server, built on
**GPUI** (Zed's GPU-accelerated UI framework) via the in-house **FluentGUI**
Fluent 2 design layer — the same stack as `rdpapp`.

Windows is the primary deployment target; it builds and runs on Linux too
(used for CI and local dev).

## Why GPUI / FluentGUI

The workspace already vendors a GPUI fork at `../../FluentGUI` and ships a
proven native app (`../../rdpapp`) on it. The terminal emulator, the
`fontdue` glyph rasterizer, and the tokio↔GPUI bridge are all ported from
there rather than reinvented, so "borrow GUI components from Zed" is satisfied
by reusing the established in-house stack.

## Architecture

```
GPUI main thread (UI)            background tokio runtime (I/O)
─────────────────────            ──────────────────────────────
RootView (sidebar + status)  ──▶ ApiClient (reqwest, rustls)
  └─ TerminalView            ──▶   REST: /api/sessions, files, git
       ├─ TermProcessor (vte)      SSE:  /api/events
       └─ TermRenderer (fontdue) ◀─  WS:  /api/sessions/{id}/attach
                                       (ws::spawn_attach)
```

- All server I/O runs on a multi-threaded tokio runtime (`bridge`). UI code
  launches work with `cx.spawn` → `tokio_handle.spawn(fut).await` →
  `weak.update(..; cx.notify())`, the rdpapp bridge pattern.
- The terminal grid/parser/renderer are GPUI-free and unit-tested.
- The PTY attach speaks the server's exact WebSocket protocol: an `auth` frame
  first, then `snapshot-start` → binary scrollback → `snapshot-done` → live
  binary frames; `resize`/`ping` as JSON control frames.

## Modules

| Module | Responsibility |
|---|---|
| `protocol` | Wire types mirroring `server/src/{pty,files,git,events,activity}.rs` |
| `client`   | REST + SSE `ApiClient` (reqwest/rustls) |
| `ws`       | PTY attach over `tokio-tungstenite` (auth-first, snapshot replay) |
| `terminal::grid` | VT100/ANSI emulator (`vte`), ported from rdpapp |
| `terminal::keymap` | Keystroke → byte-sequence mapping (pure, tested) |
| `terminal::renderer` | `fontdue` → BGRA frame |
| `bridge`   | Background tokio runtime + `spawn` |
| `config`   | Persisted server URL + token |
| `ui`       | GPUI `RootView` + `TerminalView` (behind the `gui` feature) |

## Build / test

```bash
# Core logic only — fast, no GPUI graph:
cargo test --no-default-features

# Full app (compiles GPUI + FluentGUI):
cargo run            # debug
cargo build --release
```

## Configuration

On first run the client reads/writes
`~/.config/mydevenv2-client/config.json` (`%APPDATA%\mydevenv2-client\config.json`
on Windows):

```json
{
  "server_url": "https://mydevenv2.sprooty.com",
  "token": "<MYDEVENV2_TOKEN>"
}
```

Until in-app settings land (see backlog), set the token by editing this file.

## Status

- [x] Protocol types, REST/SSE/WS client, VT100 core, fontdue renderer (tested)
- [x] GPUI shell: session sidebar, create/attach, live terminal, input
- [ ] In-app settings (token entry)
- [ ] SSE-driven live session/activity updates + real TabStrip
- [ ] File-tree / editor / git / diff tabs
- [ ] Windows installer via Woodpecker CI → download host
