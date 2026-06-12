# MyDevEnv2 - Uplift Backlog

Review conducted 2026-05-28. This combines an independent three-aspect stack
review with the existing `uplift.md`. Items that no longer matched the code
were corrected rather than carried forward verbatim.

---

## Stack-Level Uplift Additions

### 1. Dependency and Toolchain Currency

- [ ] **Frontend and mobile packages are a major generation behind** (`web/package.json`, `mobile/package.json`)
  `pnpm outdated` on 2026-05-28 shows Vite `6.4.2 -> 8.0.14`, Capacitor
  `7.6.5 -> 8.3.4`, `@xterm/xterm` `5.5.0 -> 6.0.0`, TypeScript
  `5.9.3 -> 6.0.3`, and `@solidjs/router` `0.15.4 -> 0.16.1`.
  Fix as one planned upgrade with web build/typecheck, Capacitor sync, APK
  build, terminal attach, push registration, and PWA install smoke tests.

- [ ] **Rust dependency currency needs a deliberate pass** (`server/Cargo.toml`)
  Axum/Tokio/Tower are current in the lockfile, but several direct deps have
  newer majors/minors worth reviewing: `tokio-tungstenite` `0.24 -> 0.29`,
  `web-push` `0.10 -> 0.11`, `portable-pty` `0.8 -> 0.9`, and `toml`
  `0.8 -> 1.1`. Treat this as compatibility work, not a blind `cargo update`.

- [ ] **Mobile CI allows lockfile drift** (`.woodpecker.yml:89`)
  `pnpm install --frozen-lockfile=false` can hide dependency changes in the APK
  job. After the Capacitor upgrade, commit the mobile lockfile and switch this
  back to `--frozen-lockfile`.

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

- [ ] **Best-effort installs hide missing functionality** (`Dockerfile:125-126`, `Dockerfile:157`)
  Selkies, `uv`, and `ruff` can fail without breaking the image. Either make
  them required and fail fast, or mark the feature disabled and surface that
  state through health/config so the GUI/tooling gap is obvious.

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

- [ ] **Symlink escape in workspace file/git APIs** (`server/src/files.rs:27-45`, `server/src/git.rs:21-36`)
  The guards reject `..` components but do not prevent a symlink inside
  `workspace_root` from pointing outside the workspace. Canonicalize existing
  paths after resolution and assert they still start with `workspace_root`; for
  writes, canonicalize the parent directory or use a capability/openat-style API.

- [ ] **`CorsLayer::very_permissive()`** (`server/src/app.rs:103`)
  Allows any origin with any headers. Lock this to the deployed origin plus the
  Vite dev origin (`mydevenv2.sprooty.com`, `localhost:5173`) and keep the
  allowed methods/headers narrow.

- [ ] **Session cwd validation is separate from file/git validation** (`server/src/sessions.rs:35-57`)
  Session cwd handling canonicalizes and checks `workspace_root`, while file/git
  use separate component-only guards. Extract one shared workspace path policy
  so later changes do not diverge by endpoint.

### Performance / Correctness

- [ ] **512 MiB downloads read entirely into memory** (`server/src/files.rs:127-131`)
  `tokio::fs::read` buffers the full file before responding. Use
  `tokio::fs::File` plus a streaming body for bounded memory use.

- [ ] **PTY reader thread panics on spawn failure** (`server/src/pty.rs:264-293`)
  `.expect("spawn pty reader thread")` takes down the process. Return a
  `Result` from reader setup and fail only that session.

- [ ] **One activity ticker wakes every session every 500 ms** (`server/src/pty.rs:320-333`)
  This is fine for a handful of shells but becomes background churn as session
  count grows. Replace with event/timeout-driven state updates or centralize the
  ticker.

### Quality

- [ ] **VAPID `subject` is hardcoded** (`server/src/push.rs:342-346`)
  `"mailto:sprooty@sprooty.com"` is baked in. Move it to config/env.

- [ ] **Duplicate path-validation logic** (`server/src/git.rs:21-36`, `server/src/files.rs:27-45`)
  `safe_under()` and `safe_resolve()` are near-identical. Extract a shared
  helper after fixing the symlink policy.

- [ ] **Custom base64 is reinvented twice** (`server/src/files.rs:385-418`, `server/src/api.rs:98-132`)
  The `base64` crate is already a dependency. Use it for snapshots and file
  responses.

- [ ] **Session names have no length cap** (`server/src/api.rs:46-56`, `server/src/sessions.rs:28-31`)
  Unbounded names can pollute logs and UI state. Add a 256-byte cap and trim
  whitespace consistently on create/rename.

---

## Frontend (Solid / Vite PWA)

### Security

- [ ] **Bearer token exposed in WebSocket URL** (`web/src/api.ts:315-318`)
  `?token=<bearer>` leaks into server access logs, proxy logs, and browser
  history. Switch to a WebSocket auth frame, e.g. first client text frame
  `{"type":"auth","token":"..."}`, before accepting PTY input.

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

- [ ] **`android:allowBackup="true"`** (`mobile/android/app/src/main/AndroidManifest.xml:5`)
  ADB/cloud backup can capture WebView state, including localStorage tokens. Set
  `android:allowBackup="false"` or define backup rules that exclude sensitive
  WebView/app data.

- [ ] **File provider exposes broad external storage** (`mobile/android/app/src/main/res/xml/file_paths.xml:3-4`)
  `<external-path path="." />` grants from the external storage root. Narrow it
  to the specific directory the app needs, or remove the provider if unused.

### Build / Release

- [ ] **Release build has shrinking disabled** (`mobile/android/app/build.gradle:20-24`)
  Enable R8/minification for release builds and verify ProGuard rules preserve
  Capacitor/Firebase classes.

- [ ] **CI publishes a debug APK only** (`.woodpecker.yml:78-125`)
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
| 1 | Bearer token in WebSocket URL query string | Frontend |
| 2 | Symlink escape in workspace file/git APIs | Backend |
| 3 | One bearer token controls a Docker-socket dev pod | Stack / Ops |
| 4 | `android:allowBackup` exposes stored auth state | Mobile |
| 5 | `CorsLayer::very_permissive()` | Backend |
| 6 | 512 MiB downloads buffered fully in RAM | Backend |
| 7 | Runtime image reproducibility and live install scripts | Stack / Supply chain |
| 8 | Frontend/mobile major dependency uplift | Stack / Dependencies |
| 9 | SSE reconnection has no exponential backoff | Frontend |
| 10 | PTY reader thread panics on spawn failure | Backend |
| 11 | Selkies/tooling installs can fail silently | Stack / Runtime |
| 12 | `google-services.json` needs FCM verification | Mobile |

## Merge Notes

- The previous "Monaco loads eagerly" item was stale; `Editor.tsx` already uses
  dynamic import. The retained Monaco item is now about duplicated loaders and
  disabled workers.
- The previous "dead createEffect" reference pointed at `App.tsx`; the dead
  effect is in `Terminal.tsx`.
- The previous session tab-race item was not retained because the cleanup effect
  tracks only `sessionsStore.ready` and did not reproduce from code review.
