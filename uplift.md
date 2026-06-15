# MyDevEnv2 - Uplift Backlog

Review conducted 2026-05-28 and refreshed after the June 2026 deploy/client
work. This combines an independent three-aspect stack review with the existing
`uplift.md`. Items that no longer matched the code were corrected rather than
carried forward verbatim.

---

## Stack-Level Uplift Additions

### 1. Dependency and Toolchain Currency

- [x] **Frontend and mobile package major-generation uplift** (resolved June 2026)
  `web/` and `mobile/` are on the newer Vite 8 / Capacitor 8 / xterm 6 /
  TypeScript 6 generation. Keep future upgrades deliberate and verify web
  build/typecheck, Capacitor sync, APK build, terminal attach, push
  registration, and PWA install smoke tests.

- [ ] **Rust dependency currency needs a deliberate pass** (`server/Cargo.toml`)
  Axum/Tokio/Tower are current in the lockfile, but several direct deps have
  newer majors/minors worth reviewing: `tokio-tungstenite` `0.24 -> 0.29`,
  `web-push` `0.10 -> 0.11`, `portable-pty` `0.8 -> 0.9`, and `toml`
  `0.8 -> 1.1`. Treat this as compatibility work, not a blind `cargo update`.

- [x] **Mobile CI lockfile drift** (resolved June 2026)
  `.woodpecker/server.yml` uses `pnpm install --frozen-lockfile` for the APK
  job.

### 2. Runtime Image Reproducibility and Supply Chain

- [ ] **Runtime image depends on live install scripts and floating package feeds** (`Dockerfile:56-157`)
  The image installs from NodeSource, Docker, GitHub CLI, Tailscale, Infisical,
  rclone, rustup, cargo, pip, and GitHub release URLs at build time. Pin image
  digests/tool versions, checksum downloaded tarballs where practical, and make
  essential install failures fail the build.

- [x] **Boot-time CLI bootstrap mutated the bind-mounted home** (resolved 2026-06-12)
  Container startup no longer installs Codex, Claude, or other agent-specific
  clients. Optional clients are user-managed; service credentials are brokered
  on demand by the image-baked `mydevenv2-agent-auth` helper.

- [x] **Best-effort installs hide missing functionality** (resolved June 2026)
  `uv` and `ruff` now fail the image build if they do not install. Selkies is
  pinned; if installation fails, the image records `{"selkies":null}` in
  `/etc/mydevenv2/features.json` and `/api/config` exposes that feature state.

### 3. Operational Boundaries and Observability

- [ ] **One bearer token controls a high-privilege dev pod** (`server/src/auth.rs`, `deploy/docker-compose.yml:31`)
  The same token grants PTY access, file writes, git inspection, GUI launch, and
  access to a pod with the host Docker socket mounted. Add request IDs, audit
  logging for mutating endpoints, auth-failure rate limiting, and consider
  optional capability-scoped tokens.

- [ ] **Healthcheck only proves HTTP liveness** (`deploy/docker-compose.yml:56-61`)
  `/healthz` does not verify workspace mount health, push store access,
  Tailscale state, Sway, or GUI stream readiness. Add a readiness endpoint with
  component checks and keep `/healthz` as a cheap process-liveness probe.

- [ ] **Docker socket mount should be an explicit risk boundary** (`deploy/docker-compose.yml:31`)
  DooD is useful here, but a server compromise becomes host-level control.
  Document it in deploy docs and consider an optional socket proxy or a disabled
  default for deployments that do not need Docker from inside the pod.

---

## Backend (Rust / Axum)

### Security

- [x] **Symlink escape in workspace file/git APIs** (resolved June 2026)
  Filesystem endpoints now route through `server/src/workspace_path.rs`, which
  canonicalizes existing paths, checks the canonical path remains under
  `workspace_root`, and canonicalizes write parents before joining the final
  filename.

- [x] **`CorsLayer::very_permissive()`** (resolved June 2026)
  CORS is now built from `MYDEVENV2_ALLOWED_ORIGINS` / config, defaulting to the
  production origin plus Vite dev origins with a narrow method/header list.

- [ ] **Session cwd validation is separate from file/git validation** (`server/src/sessions.rs:35-57`)
  Session cwd handling canonicalizes and checks `workspace_root`, while file/git
  use separate component-only guards. Extract one shared workspace path policy
  so later changes do not diverge by endpoint.

### Performance / Correctness

- [x] **512 MiB downloads read entirely into memory** (resolved June 2026)
  `/api/files/download` streams through `tokio_util::io::ReaderStream` with a
  512 MiB transfer cap. Editor reads still use the smaller 5 MiB in-memory cap.

- [ ] **PTY reader thread panics on spawn failure** (`server/src/pty.rs:264-293`)
  `.expect("spawn pty reader thread")` takes down the process. Return a
  `Result` from reader setup and fail only that session.

- [ ] **One activity ticker wakes every session every 500 ms** (`server/src/pty.rs:320-333`)
  This is fine for a handful of shells but becomes background churn as session
  count grows. Replace with event/timeout-driven state updates or centralize the
  ticker.

### Quality

- [x] **VAPID `subject` is hardcoded** (resolved June 2026)
  `vapid_subject` is configurable via TOML or `MYDEVENV2_VAPID_SUBJECT`.

- [x] **Duplicate path-validation logic** (resolved June 2026)
  Shared policy lives in `server/src/workspace_path.rs`.

- [ ] **Custom base64 is reinvented twice** (`server/src/files.rs:385-418`, `server/src/api.rs:98-132`)
  The `base64` crate is already a dependency. Use it for snapshots and file
  responses.

- [ ] **Session names have no length cap** (`server/src/api.rs:46-56`, `server/src/sessions.rs:28-31`)
  Unbounded names can pollute logs and UI state. Add a 256-byte cap and trim
  whitespace consistently on create/rename.

---

## Frontend (Solid / Vite PWA)

### Security

- [x] **Bearer token exposed in WebSocket URL** (resolved June 2026)
  Web and native clients now send first-frame WebSocket auth. The server keeps
  legacy `?token=` support only for older clients.

- [ ] **Stored token has no cross-tab logout/clear path** (`web/src/api.ts:39-48`, `web/src/Settings.tsx:37-47`)
  Clearing the token in one tab reloads only that tab. Add a logout/clear action
  that removes localStorage state and broadcasts through `BroadcastChannel` so
  all tabs disconnect and clear state together.

### Correctness

- [ ] **SSE reconnection is fixed at 2 s** (`web/src/store.ts:122-136`)
  Sustained outages cause every open client to retry on the same short interval.
  Use exponential backoff with jitter and a cap around 30 s.

- [ ] **Blank terminal on mid-snapshot disconnect** (`web/src/Terminal.tsx:291-333`)
  If the socket closes while `inSnapshot` is true, the UI suppresses the
  disconnected message and may leave a blank terminal. Show a reconnecting
  overlay/status for interrupted snapshots.

- [ ] **Service worker only handles push** (`web/public/sw.js:1-52`)
  The app intentionally has no offline mode, but installed PWAs benefit from at
  least a versioned app-shell cache or an explicit offline fallback. Either add
  that cache or document the online-only behavior in the app/release notes.

### Performance

- [ ] **Git diff editor thrashes on path change** (`web/src/Git.tsx:104-111`)
  `host.innerHTML = ""` and full diff-editor re-init run on path changes.
  Reuse the Monaco diff editor and call `setModel()` with disposed old models.

- [ ] **Monaco loaders are duplicated and use no-op workers** (`web/src/Editor.tsx:17-38`, `web/src/Git.tsx:19-35`)
  The editor is already lazy-loaded, but the loader exists twice and disables
  language workers. Extract one Monaco loader and decide whether richer workers
  are worth shipping.

### Quality

- [ ] **Dead `createEffect` block** (`web/src/Terminal.tsx:342-348`)
  The effect tracks `props.sessionId` but intentionally does nothing. Remove it
  unless the parent can actually reuse a mounted `Terminal` for a different id.

---

## Mobile (Capacitor / Android)

### Security

- [x] **Android backup can capture stored auth state** (resolved June 2026)
  `android:allowBackup="false"` and Android 12+ data extraction rules exclude
  root/file/database/sharedpref/external data because WebView storage contains
  the bearer token.

- [x] **File provider exposes broad external storage** (resolved June 2026)
  The provider is narrowed to a cache subdirectory only.

### Build / Release

- [ ] **Release build has shrinking disabled** (`mobile/android/app/build.gradle:20-24`)
  Enable R8/minification for release builds and verify ProGuard rules preserve
  Capacitor/Firebase classes.

- [ ] **CI publishes a debug APK only** (`.woodpecker/server.yml`)
  The release script exists locally, but CI builds and uploads `assembleDebug`.
  Add signed release build config before treating the APK as production-grade.

- [ ] **`google-services.json` contains a stale first client entry**
  The file includes `com.sprooty.mydevenv2`, but the first client entry is still
  `android.desktop.client`. Regenerate from Firebase Console or verify the
  Gradle plugin selects the matching client, then confirm FCM registration on a
  real device.

---

## Priority Order

| # | Finding | Layer |
|---|---------|-------|
| 1 | One bearer token controls a Docker-socket dev pod | Stack / Ops |
| 2 | Runtime image reproducibility and live install scripts | Stack / Supply chain |
| 3 | SSE reconnection has no exponential backoff | Frontend |
| 4 | PTY reader thread panics on spawn failure | Backend |
| 5 | Release APK signing/shrinking + production-grade Android distribution | Mobile |
| 6 | `google-services.json` / FCM real-device verification | Mobile |
| 7 | Health/readiness should cover workspace, push store, Tailscale, GUI stream | Stack / Ops |
| 8 | Audit logging and optional capability-scoped auth tokens | Stack / Ops |

## Merge Notes

- The previous "Monaco loads eagerly" item was stale; `Editor.tsx` already uses
  dynamic import. The retained Monaco item is now about duplicated loaders and
  disabled workers.
- The previous "dead createEffect" reference pointed at `App.tsx`; the dead
  effect is in `Terminal.tsx`.
- The previous session tab-race item was not retained because the cleanup effect
  tracks only `sessionsStore.ready` and did not reproduce from code review.
