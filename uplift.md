# MyDevEnv2 Outstanding Uplift Backlog

This is the single canonical backlog for remaining uplift work in MyDevEnv2.
Completed uplift work lives in normal repo history, README status notes, and
the code itself. No other markdown file should carry a parallel open-items
backlog.

---

## Stack / Ops

1. **Rust dependency currency pass** (`server/Cargo.toml`)
   Review and deliberately upgrade aging direct dependencies such as
   `tokio-tungstenite`, `web-push`, `portable-pty`, and `toml`.

2. **Runtime image reproducibility hardening** (`Dockerfile`)
   Replace floating install-script/package-feed assumptions with more deliberate
   pinning and checksum validation where practical.

3. **Bearer-token risk boundary** (`server/src/auth.rs`, `deploy/docker-compose.yml`)
   One token still gates PTY access, file writes, git inspection, GUI launch,
   and a Docker-socket-mounted pod. Add stronger audit/rate-limit/capability
   controls.

4. **Readiness beyond liveness** (`deploy/docker-compose.yml`, server health endpoints)
   Add a readiness endpoint that checks workspace mount health, push store
   access, Tailscale state, and GUI-related dependencies instead of relying
   only on `/healthz`.

5. **Docker socket boundary documentation / optional isolation** (`deploy/`)
   Keep the current DooD behavior explicit in operator docs and consider a
   socket proxy or disabled-by-default pattern for lower-privilege deployments.

6. **Production stack cleanup for exit-node config** (live Komodo stack / ops repo)
   The live stack still carries a stale `TAILSCALE_EXIT_NODE=hertzde3` setting
   that causes startup warnings before the app continues without an exit node.

---

## Backend

1. **Unify session cwd validation with shared workspace path policy**
   (`server/src/sessions.rs`, `server/src/workspace_path.rs`)

2. **Handle PTY reader spawn failure without panicking**
   (`server/src/pty.rs`)

3. **Replace per-session 500 ms activity tickers with a lower-churn model**
   (`server/src/pty.rs`)

4. **Replace custom base64 helpers with the `base64` crate**
   (`server/src/files.rs`, `server/src/api.rs`)

5. **Add a session-name length cap and consistent trimming**
   (`server/src/api.rs`, `server/src/sessions.rs`)

---

## Frontend / PWA

1. **Decide and implement installed-PWA offline behavior**
   (`web/public/sw.js`, app/release notes)

2. **Reuse the Monaco diff editor instead of reinitializing on each path change**
   (`web/src/Git.tsx`)

3. **Consolidate Monaco loading and revisit worker behavior**
   (`web/src/Editor.tsx`, `web/src/Git.tsx`)

4. **Richer push-notification controls**
   Per-session/task rules, quiet hours, and digests.

5. **Mature the GUI tab**
   Saved launchers, process labeling, and stream health visibility.

6. **Improve mobile ergonomics in the web surface**
   Reconnect flows, tab management, terminal use, and touch-first editing.

7. **Improve auth and onboarding UX**
   Bearer token setup and device-local profile handling.

8. **Add lightweight admin / operational visibility in-app**
   Sessions, push, GUI, auth-broker, and storage state.

9. **User-configurable retention and quota controls**
   History, scrollback, prompts, and storage budgets.

10. **Workspace search improvements**
   Filename/symbol search and tighter editor integration.

11. **Stronger workspace awareness**
   Detected projects, task-runner shortcuts, and language-specific quick
   actions.

---

## Mobile / Android

1. **Enable R8 / shrinking for release builds**
   (`mobile/android/app/build.gradle`)

2. **Produce a signed release APK in CI**
   (`.woodpecker/server.yml`)

3. **Regenerate or verify `google-services.json` client selection**
   Confirm real-device FCM registration for `com.sprooty.mydevenv2`.

---

## Product / Codebase Hygiene

1. **Packaging and release polish across server/PWA/mobile**

2. **Module and product-surface boundary cleanup across server, web, mobile,
   and deprecated client code**

3. **Operator and user documentation consolidation to reduce drift**

---

## Suggested Priority

1. Bearer-token risk boundary
2. Runtime image reproducibility hardening
3. PTY reader spawn-failure handling
4. Android release/shrinking/signing
5. FCM / `google-services.json` verification
6. Readiness beyond liveness
7. Production stack exit-node cleanup
