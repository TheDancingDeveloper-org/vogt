# MyDevEnv2 — Intent

A from-scratch redesign of MyDevEnv. Same underlying problem; cleaner approach informed by what MyDevEnv (v1) got right and wrong.

## What I'm trying to achieve

A single, centrally-hosted development environment I can drive from a browser on any device — phone, tablet, laptop — over Tailscale. My day-to-day work is interactive sessions with **Claude Code** and **Codex**: lots of long-running agent terminals producing streaming output, with occasional permission/approval prompts I need to respond to quickly. I want to start an agent on the desktop, walk away, and pick it up on my phone with full scrollback and the ability to type back.

It needs to handle three workloads:

1. **Terminals (primary)** — many concurrent named PTY sessions for Claude Code, Codex, builds, watchers. Server-owned so they survive client disconnects and broadcast to multiple attached clients.
2. **Light editing + git** — file tree, Monaco-based editor as a tab type, git status/diff. Not a full IDE.
3. **GUI work (occasional)** — testing web/Angular/React/Slint apps in a real browser, and running the Android Studio emulator for Android dev. These appear as embedded tabs alongside terminals so it's one pane of glass.

## What I do not want

- A code-server fork. v1 ended up here and the maintenance burden killed momentum. Embedding Monaco directly is enough editing capability without inheriting the VS Code integration nightmare.
- tmux as the multiplexing primitive. Keybindings are too dense; the server owning sessions and the UI providing mouse/touch tab switching gives the same outcome with no keybinding tax.
- Per-project isolation / ephemeral environments. I do all my dev in `~/Working` with 50+ repos checked out; the directory boundary is the project boundary. One persistent pod is the right unit.
- Public internet exposure. Tailscale-only. The threat model is "convenient access from my own devices," not "share with collaborators."
- A second native codebase for mobile. A responsive PWA wrapped in Capacitor is the mobile story.

## Why a rewrite rather than fixing MyDevEnv v1

v1 has the right core (Rust/Axum server that owns PTYs, broadcasts to clients, exposes file + git APIs) but accumulated too many half-finished surface layers — a code-server fork, a Tauri desktop, a React Native mobile app, a separate native APK wrapper, two web UIs. The foundation is salvageable but the directory is hard to navigate and the surface is hard to reason about. v2 starts clean, keeps the server primitives in spirit, and builds exactly one client UI (responsive web + Capacitor wrap).

## Success criteria

- I can open one Tailscale URL on phone or laptop and see all my active agent sessions.
- A Claude Code prompt waiting for approval is visible from the tab strip without clicking into the tab (activity badge + optional push notification).
- Reconnecting from any device shows the last N lines of every session immediately — no waiting for output to redraw.
- Editing a file, viewing git diff, and running a GUI app under test all happen in tabs in the same UI.
- The Android emulator can be started from the UI, used, and stopped without leaving the browser.
- Mobile usage is not painful: soft keyboard works, modifier keys are accessible, network flaps reconnect cleanly.
