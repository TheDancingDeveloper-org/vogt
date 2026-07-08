# MyDevEnv2 Outstanding Uplift Backlog

This is the single canonical backlog for remaining uplift work in MyDevEnv2.
Completed uplift work lives in normal repo history, README status notes, and
the code itself. No other markdown file should carry a parallel open-items
backlog.

---

## Stack / Ops

1. **Runtime image reproducibility hardening** (`Dockerfile`)
   The image and Rust CI steps now pin and checksum-verify several downloaded
   tool archives, pin the ad-hoc `cargo install` tool set, and avoid the old
   NodeSource / Infisical shell-pipe installers. Remaining work is tightening
   still-floating third-party apt feeds and validating the full image build on
   a BuildKit-capable Docker host.

2. **Bearer-token risk boundary** (`server/src/auth.rs`, `deploy/docker-compose.yml`)
   Repo-side audit logging, per-token mutation rate limits, and scoped token
   capabilities now exist. The remaining work is provisioning real non-admin
   tokens in production, moving the live clients onto them where appropriate,
   and deciding whether the primary token should keep full Docker-adjacent
   access long term.

3. **Promote `/readyz` into the live stack and validate it on Node B**
   Repo-side readiness checks now cover workspace mount health, state-dir
   writability, Tailscale state, and GUI dependencies. The remaining work is
   syncing the compose healthcheck into `ops` and validating the live stack.

4. **Production stack cleanup for exit-node config** (live Komodo stack / ops repo)
   The live stack still carries a stale `TAILSCALE_EXIT_NODE=hertzde3` setting
   that causes startup warnings before the app continues without an exit node.

---

## Backend

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

---

## Suggested Priority

1. Bearer-token risk boundary
2. Runtime image reproducibility hardening
3. Woodpecker Android signing secret provisioning
4. Real-device native FCM verification
5. Live `/readyz` rollout validation
6. Production stack exit-node cleanup
