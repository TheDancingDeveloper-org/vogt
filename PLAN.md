# MyDevEnv2 — Plan

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│  Node B (Tailscale-exposed)                                 │
│                                                             │
│  ┌──────────────────────────────────────────────┐           │
│  │  Persistent dev pod (Docker / Komodo stack)  │           │
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
                ┌─────────┴──────────┬─────────────────────┐
                ▼                    ▼                     ▼
        Browser (PWA)         Android (Capacitor wrap)  Native desktop
       desktop + tablet       installable APK + FCM     GPUI/FluentGUI
       (incl. iOS Safari      push; loads deployed      client, Windows
        Add-to-Home-Screen)   PWA directly              primary target
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

### Mobile wrap (Android only for MVP)

Capacitor 8 wraps the PWA into an installable Android app. Adds native FCM push,
home-screen install, and deep links. No second UI codebase: the WebView loads
`https://mydevenv2.sprooty.com` directly, with `mobile/web/` only as the
Capacitor-required fallback bundle.

**Distribution:** sideload the debug APK directly for MVP — no Play Store
listing, no signing-key registration, no review cycle. The server Woodpecker
workflow builds it with `assembleDebug` and publishes it to the Forgejo release
tag `apk-latest` as `mydevenv2-debug.apk`. The phone needs "Install unknown
apps" enabled for the source app once.

**iOS deferred.** PWA still works in Safari via Add-to-Home-Screen — that gets the install, the standalone shell, and (on iOS 16.4+ for installed PWAs) web push. What's deferred is the Capacitor iOS build / App Store distribution / APNs registration, all of which need an Apple developer account.

### Native desktop client

The native client in `client/` is an optional high-performance front end for
desktop use, not the primary deployment surface. It uses GPUI plus the in-house
FluentGUI layer, matching `rdpapp`, and speaks the same server REST/SSE/WS
protocol as the PWA.

Current scope:

- Windows is the primary release target; Linux builds are used for CI and
  parity smoke testing.
- In-app server URL/token settings are persisted to the platform config dir.
- Session list, create/attach, terminal rendering, SSE activity updates, mouse
  selection, and Ctrl+Shift+C/V are implemented.
- Full file/editor/git/GUI parity with the PWA is future work.

Releases use `client-v*` tags. `.woodpecker/client.yml` publishes the Linux
artifact and `.woodpecker/client-windows.yml` builds natively on the Windows
`arbit-win` agent, publishing the installer, portable exe, and checksums to the
same Forgejo release.

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

### Phase 6 — Push + Android Capacitor wrap (~2-3 days)
- Web push subscription on the server (works for any browser PWA; the same code path drives Android FCM via Capacitor's `PushNotifications` plugin)
- Push notification on activity transitions to `waiting-for-input`
- Capacitor wrap producing a sideloadable Android APK (no Play Store listing for MVP)
- iOS is intentionally out of scope this phase — PWA in Safari still works

### Native desktop client — added after Phase 6
- GPUI/FluentGUI shell and native terminal core
- In-app server settings, session list/create/attach, SSE live updates
- Linux CI and native Windows release workflow on client tags

### Phase 7 — Android emulator VM (~1 week, pending)
- libvirt VM template
- Start/stop API in server, button in UI
- Selkies inside the VM
- Android tab type
- ADB-over-network from dev pod

## Out of scope (v2)

- Per-project isolated environments
- Public exposure / multi-user / team features
- **iOS Capacitor build / App Store distribution / APNs** — requires Apple developer account. iOS users get the PWA via Safari Add-to-Home-Screen (with web push on 16.4+ when installed).
- **Android Play Store listing** — sideloaded APK is the MVP distribution; revisit only if I want it on someone else's phone.
- VS Code extension ecosystem (use Monaco directly; if I ever need extensions, run upstream code-server unmodified on a separate port and link out)
- Real-time collaborative editing
- Embedded language servers (LSP can come later as a tab feature if needed; Claude/Codex are the primary "intelligence" layer)

## Resolved decisions

- **Container runtime:** Docker image deployed by Komodo on Node B.
- **PWA framework:** Solid + Vite + TypeScript.
- **Deployment:** Forgejo push → Woodpecker build → Forgejo registry image →
  ops compose SHA bump → Komodo `DeployStack`.
- **Mobile distribution:** sideloaded Android APK from Forgejo release
  `apk-latest`; iOS remains browser/PWA-only.
- **Desktop client:** optional GPUI/FluentGUI native client, released from
  `client-v*` tags.
- **Service auth:** production default interactive sessions run through
  `mydevenv2-agent-auth` using a read-only Infisical Universal Auth identity.

## Remaining decisions

- **GUI stream activation:** Sway and Selkies are installed in the image, but
  production still has `START_SWAY=0` and `GUI_STREAM_URL=""`. Enable and
  verify Selkies before treating the GUI tab as operational.
- **Android emulator VM:** Phase 7 still needs the libvirt/KVM VM, display
  stream, ADB bridge, and UI controls.
- **FCM real-device verification:** server-side FCM HTTP v1 and Capacitor token
  registration are implemented, but push delivery still needs a real-device
  smoke test with the production service-account JSON.
