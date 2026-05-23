# MyDevEnv2 — Plan

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│  Node B (Tailscale-exposed)                                 │
│                                                             │
│  ┌──────────────────────────────────────────────┐           │
│  │  Persistent dev pod (LXC container)          │           │
│  │  ─ /home/sprooty/Working bind-mounted        │           │
│  │  ─ MyDevEnv2 server (Rust / Axum)            │           │
│  │     • owns all PTYs                          │           │
│  │     • server-side scrollback per session     │           │
│  │     • file + git APIs                        │           │
│  │     • broadcasts PTY to N attached clients   │           │
│  │     • per-session activity flags             │           │
│  │     • web push for prompt-waiting events     │           │
│  │  ─ Sway compositor (headless)                │           │
│  │  ─ Selkies-GStreamer WebRTC stream of Sway   │           │
│  │  ─ Claude Code / Codex run as PTY children   │           │
│  └──────────────────────────────────────────────┘           │
│                                                             │
│  ┌──────────────────────────────────────────────┐           │
│  │  On-demand KVM VM: android-dev               │           │
│  │  ─ /dev/kvm passthrough                      │           │
│  │  ─ Android Studio + emulator                 │           │
│  │  ─ Selkies/KasmVNC for display               │           │
│  │  ─ start/stop API surfaced in MyDevEnv2 UI   │           │
│  └──────────────────────────────────────────────┘           │
└─────────────────────────────────────────────────────────────┘
                          ▲
                          │ Tailscale only
                          │
                ┌─────────┴──────────┐
                ▼                    ▼
        Browser (PWA)         Mobile (Capacitor wrap)
       desktop + tablet       iOS + Android, push notifications
```

## Components

### Server (Rust / Axum)

Single binary. Salvages design ideas from MyDevEnv v1's `server/` but written fresh — clean module boundaries, no legacy compatibility.

Responsibilities:
- PTY lifecycle: create / list / attach / detach / kill named sessions
- Server-side scrollback buffer per session (configurable size, e.g. 10k lines)
- Activity tracking: each session flags "idle / running / waiting-for-input / errored"
- File APIs: read, write, list, tree, search (uses ripgrep)
- Git APIs: status, diff, log, branch — read-only initially
- WebSocket multiplex for PTY attach (multiple clients per session)
- Web push subscription + notification dispatch on activity transitions
- KVM control API: list/start/stop named libvirt domains (Android VM)
- Auth: Tailscale-only deployment, plus a bearer token for the WebSocket/HTTP API

Explicitly **not** in the server: editor logic, language servers, terminal rendering. All client concerns.

### Web client (responsive PWA)

Single UI for all devices. Three breakpoints (phone / tablet / desktop), same component tree.

Tab types:
- **Terminal** — xterm.js, attached to a server PTY. Custom input layer for mobile IME compatibility.
- **Editor** — Monaco editor, file content over HTTP, save via PUT.
- **Diff** — Monaco diff view over git API output.
- **GUI** — Selkies WebRTC stream of the Sway compositor.
- **Android** — Selkies/VNC stream of the on-demand Android VM.

Tab strip is first-class:
- Per-tab activity badge (waiting / running / idle)
- Drag to reorder, right-click / long-press to rename, close, duplicate
- Deep-linkable URLs per tab (`/t/<session-id>`)

Mobile-specific UX:
- Pinned modifier-key row above soft keyboard (Ctrl / Esc / Tab / arrows / Enter)
- Collapsible drawer for file tree + git
- Wake lock while a terminal is focused
- Web push notification when a watched session goes `waiting-for-input`

### Mobile wrap

Capacitor wraps the PWA. Adds native push notifications, proper home-screen install on iOS, deep links. No second codebase.

### GUI layer (Sway + Selkies)

Sway runs headless inside the dev pod. Selkies-GStreamer exposes a WebRTC stream consumable by the web client. Any GUI app launched in the pod (Chromium for testing web UIs, Slint apps for rdpapp dev, etc.) renders on Sway and streams to the GUI tab.

Selkies chosen over KasmVNC for lower latency and better mobile browser support. Fallback to KasmVNC if Selkies setup proves fragile.

### Android emulator (KVM VM)

Separate libvirt VM with `/dev/kvm` passthrough. Not part of the persistent pod — started on demand from the MyDevEnv2 UI, stopped when idle to reclaim host resources. Display via Selkies inside the VM, exposed as its own tab type. ADB-over-network back to the dev pod so builds from the pod deploy straight to the emulator.

## Build order

Sequenced to deliver a usable system as fast as possible and defer the fiddliest parts.

### Phase 1 — Server foundation (~1 week)
- Project skeleton: Axum, structured config (TOML + env), bearer-token auth
- PTY session store with server-side scrollback ring buffers
- WebSocket attach/broadcast
- Activity state machine per session (idle/running/waiting/errored — heuristics on output patterns and time-since-output)
- HTTP API: list sessions, create, kill, rename, attach
- Smoke test with `websocat` from CLI

### Phase 2 — Web UI MVP, terminals only (~1-2 weeks)
- Responsive shell: tab strip + main pane + drawer
- xterm.js terminal tab with custom mobile input layer
- Modifier-key row for mobile
- Activity badges in tab strip
- Deep-link URLs
- Deployed to Node B behind Tailscale, dogfood it

### Phase 3 — File tree + editor (~3-4 days)
- File tree drawer with lazy expand
- Monaco editor tab type
- Read/write via server file APIs
- Search (ripgrep)

### Phase 4 — Git tab (~2-3 days)
- Status view, diff view (Monaco diff), log
- Read-only — commits still happen via terminal (Claude/Codex own the workflow)

### Phase 5 — GUI streaming (~3-4 days)
- Sway in the dev pod
- Selkies-GStreamer configured and exposed
- GUI tab type in the web UI
- Test with Chromium running an Angular dev server

### Phase 6 — Push + Capacitor wrap (~2-3 days)
- Web push subscription on the server
- Push notification on activity transitions to `waiting-for-input`
- Capacitor wrap for iOS + Android installable apps

### Phase 7 — Android emulator VM (~1 week)
- libvirt VM template
- Start/stop API in server, button in UI
- Selkies inside the VM
- Android tab type
- ADB-over-network from dev pod

## Out of scope (v2)

- Per-project isolated environments
- Public exposure / multi-user / team features
- Native desktop wrapper (web UI in a browser is enough)
- VS Code extension ecosystem (use Monaco directly; if I ever need extensions, run upstream code-server unmodified on a separate port and link out)
- Real-time collaborative editing
- Embedded language servers (LSP can come later as a tab feature if needed; Claude/Codex are the primary "intelligence" layer)

## Open decisions

These don't block Phase 1 but should be answered before the relevant phase:

- **Container runtime for the dev pod**: LXC vs Docker vs Incus. Leaning LXC/Incus for systemd-like behaviour, but Docker is fine if simpler operationally on Node B.
- **PWA framework**: React, Solid, or Svelte. Solid leans fastest for the dense reactive UI; React has the largest ecosystem for xterm.js + Monaco bindings.
- **Selkies vs KasmVNC** for the GUI layer — try Selkies first, fall back if setup is too fragile.
- **Push provider** for iOS — Apple Push via Capacitor, requires Apple developer account. Android via FCM.
- **Where the server runs** — directly on Node B vs inside the dev pod itself. Inside the pod is cleaner (single artefact, single lifecycle).
