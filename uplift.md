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
   The image now pins and checksum-verifies several downloaded tool archives
   and drops the NodeSource / Infisical shell-pipe installers. Remaining work
   is tightening still-floating third-party package sources (notably some apt
   feeds and `cargo install` tools) and validating the full build on a
   BuildKit-capable Docker host.

3. **Bearer-token risk boundary** (`server/src/auth.rs`, `deploy/docker-compose.yml`)
   Repo-side audit logging, per-token mutation rate limits, and scoped token
   capabilities now exist. The remaining work is provisioning real non-admin
   tokens in production, moving the live clients onto them where appropriate,
   and deciding whether the primary token should keep full Docker-adjacent
   access long term.

4. **Promote `/readyz` into the live stack and validate it on Node B**
   Repo-side readiness checks now cover workspace mount health, state-dir
   writability, Tailscale state, and GUI dependencies. The remaining work is
   syncing the compose healthcheck into `ops` and validating the live stack.

5. **Docker socket boundary documentation / optional isolation** (`deploy/`)
   Keep the current DooD behavior explicit in operator docs and consider a
   socket proxy or disabled-by-default pattern for lower-privilege deployments.

6. **Production stack cleanup for exit-node config** (live Komodo stack / ops repo)
   The live stack still carries a stale `TAILSCALE_EXIT_NODE=hertzde3` setting
   that causes startup warnings before the app continues without an exit node.

---

## Backend

---

## Frontend / PWA

1. **Richer push-notification controls**
   Per-session/task rules, quiet hours, and digests.

2. **Improve mobile ergonomics in the web surface**
   Reconnect flows, tab management, terminal use, and touch-first editing.

3. **User-configurable retention and quota controls**
   History, scrollback, prompts, and storage budgets.

4. **Workspace search improvements**
   Filename search and direct editor-open integration now exist in the command
   palette. Remaining work is symbol-oriented search/navigation and deciding
   whether the file tree itself should adopt server-backed search for very
   large workspaces.

5. **Stronger workspace awareness**
   Detected projects, task-runner shortcuts, and language-specific quick
   actions.

---

## Mobile / Android

1. **Provision Woodpecker Android signing secrets and verify live release upload**
   The repo now supports signed release APK builds in CI; the remaining work is
   ensuring `mydevenv2_android_keystore_base64`,
   `mydevenv2_android_keystore_password`, `mydevenv2_android_key_alias`, and
   `mydevenv2_android_key_password` are present in Woodpecker and validating a
   real `apk-latest` upload from `main`.

2. **Real-device native FCM verification**
   `google-services.json` already includes the `com.sprooty.mydevenv2` client;
   the remaining work is confirming first-launch FCM registration and end-to-end
   delivery on actual Android hardware.

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
3. Woodpecker Android signing secret provisioning
4. Real-device native FCM verification
5. Live `/readyz` rollout validation
6. Production stack exit-node cleanup
