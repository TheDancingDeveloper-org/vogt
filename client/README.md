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

The client depends on the GPUI fork checked out as a sibling repo at
`../../FluentGUI` from this crate. CI pins that checkout to
`f601e54b4e58e416bc7495a75468b82af9a10545`.

```bash
# Core logic only — fast, no GPUI graph:
cargo fmt --check
cargo clippy --no-default-features --all-targets -- -D warnings
cargo test --no-default-features

# Full app (compiles GPUI + FluentGUI):
cargo run            # debug
cargo build --release
```

Windows release builds must run natively on Windows with the MSVC toolchain,
`fxc.exe`, NSIS, and the GPUI shader toolchain available. Linux
cross-compiles are useful for type/link checks only; they do not produce a
runnable Windows release binary because GPUI's release shader path precompiles
HLSL through `fxc`.

## Configuration

On first run the client opens its settings panel until both a server URL and
token are configured. The values are persisted to
`~/.config/mydevenv2-client/config.json` on Linux and
`%APPDATA%\mydevenv2-client\config.json` on Windows:

```json
{
  "server_url": "https://mydevenv2.sprooty.com",
  "token": "<MYDEVENV2_TOKEN>"
}
```

The token is sent as `Authorization: Bearer` for HTTP/SSE and as the first
WebSocket text frame (`{"type":"auth","token":"..."}`) before PTY attach
snapshot replay begins.

## CI / releases

Client CI is split from the production server deploy workflow:

- `.woodpecker/client.yml` runs on `client/**` pushes and `client-v*` tags on
  Linux. It performs core checks, full GPUI clippy, and a Linux release build.
  On tags it uploads `MyDevEnv2-Client-<tag>-linux-x86_64` plus checksums to
  the Forgejo release.
- `.woodpecker/client-windows.yml` runs on the Windows `arbit-win` agent for
  `client-v*` tags and manual runs. It executes
  `client/ci/windows/build-and-publish.ps1` from `C:\ci\mydevenv2\`, builds the
  native MSVC release, runs NSIS, and uploads:
  - `MyDevEnv2-Setup-<tag>.exe`
  - `MyDevEnv2-Client-<tag>-windows-x86_64.exe`
  - `SHA256SUMS-<tag>.txt`

The latest verified release at the time of this doc update is `client-v0.1.4`.
Both Linux and native Windows Woodpecker workflows completed successfully for
that tag.

## Status

- [x] Protocol types, REST/SSE/WS client, VT100 core, fontdue renderer (tested)
- [x] GPUI shell: session sidebar, create/attach, live terminal, input
- [x] In-app settings for server URL + token
- [x] SSE-driven live session/activity updates
- [x] Mouse selection plus Ctrl+Shift+C / Ctrl+Shift+V terminal clipboard flow
- [x] Ctrl+Space/NUL and tested terminal key mapping
- [x] Native Windows installer and portable exe via Woodpecker release tags
- [ ] Real TabStrip / multiple simultaneous terminal panes in the native UI
- [ ] File-tree / editor / git / diff tabs

## Native app improvement backlog

1. Multiple terminal tabs and panes: keep attached terminal views alive while
   switching sessions, add a real content tab strip, support close-without-kill,
   and later split panes.
2. Session lifecycle controls: expose rename, duplicate, kill, delete, cwd,
   command, and environment fields so the native client can manage sessions
   without falling back to the PWA.
3. PWA workflow parity: add native file browsing, text preview/edit, search,
   git status, log, and diff views backed by the existing typed REST client.
4. Terminal ergonomics: add scrollback search, explicit reattach after lag or
   reconnect, clear/reset controls, configurable font sizing, and clearer
   terminal status banners.
5. Native polish: protect local credentials, integrate OS keychain storage,
   add native waiting-for-input notifications, expose app/version/release
   details, and add tray or menu actions for common commands.
6. Desktop validation: keep the GPUI-free protocol and terminal tests fast,
   then add GUI smoke coverage for first-run settings, session list, attach,
   typing, resize, reconnect, and release packaging.
