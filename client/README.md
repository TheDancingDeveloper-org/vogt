# MyDevEnv2 Desktop Client

Deprecated on July 7, 2026.

This native GPUI desktop client is no longer a supported MyDevEnv2 product
surface. The supported desktop experience is the browser/PWA, and Android
remains a thin native shell over that same web client.

The `client/` tree is retained only as historical reference while the project
decides whether to replace it with a thin Windows wrapper around the shared web
UI. No active CI or release workflow remains for this client.

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
- Shared wire DTOs now live in the sibling crate `../contract` so the server
  and native client decode the same Rust contract types.

## Modules

| Module | Responsibility |
|---|---|
| `protocol` | Thin re-export of the shared `mydevenv2-contract` wire types |
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

Historical note: the CI/release details below describe the last active native
client setup. They are no longer current as of July 7, 2026.

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

Important Windows release behavior:

- Linux cross-builds are only checks. They are not shipped because GPUI's
  release shader path requires `fxc.exe` on native Windows.
- The Windows workflow does not use the current agent workspace. It has
  `skip_clone: true`; `build-and-publish.ps1` clones Forgejo itself, fetching
  `refs/tags/<client-v*>` for tag builds or `main` for manual untagged builds.
- Uncommitted local changes cannot appear in the native Windows artifacts.
  Commit and push the client changes, then push a `client-v*` tag to publish a
  Windows installer and portable exe containing those changes.
- A manual Windows workflow run without a tag builds Forgejo `main` and exits
  without publishing release assets.

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
- [x] Terminal zoom (Ctrl+wheel / buttons / Ctrl±0), persisted
- [x] Scrollback history nav (Shift+PageUp/Down, Ctrl+Home/End) + position indicator
- [x] Auto-reconnect on WS drop (OS 10054) + manual Reconnect button
- [x] Files tab: native OS upload picker, breadcrumb nav, clickable listing
- [x] Resizable + collapsible (icon-rail) sidebar, persisted
- [x] Black-on-dark contrast floor in the terminal renderer + themed panels
- [ ] Real TabStrip / multiple simultaneous terminal panes in the native UI
- [ ] In-app file editor / git diff tabs

See `UPLIFT.md` for the June 2026 GUI uplift details (maps each change to its
original request).

## Native app improvement backlog

1. Multiple terminal panes: keep attached terminal views alive while switching
   sessions and later split panes (tabs + close-without-kill already done).
2. In-app editing: file preview is read-only; add edit/save and git diff views.
3. Button tooltips: `fluent_primitives::Button` needs a `.tooltip()` builder
   (we own FluentGUI — add it there and re-pin the rev). Icon-only buttons
   currently rely on labels/context.
4. Native polish: OS keychain credential storage, waiting-for-input
   notifications, app/version/release surface, tray/menu actions.
5. Desktop validation: keep the GPUI-free protocol/terminal tests fast; add GUI
   smoke coverage for first-run settings, attach, typing, resize, reconnect,
   upload, and release packaging.
