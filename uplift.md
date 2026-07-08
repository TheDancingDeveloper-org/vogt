# MyDevEnv2 Outstanding Uplift Backlog

This is the single canonical backlog for remaining uplift work in MyDevEnv2.
Completed uplift work lives in normal repo history, README status notes, and
the code itself. No other markdown file should carry a parallel open-items
backlog.

---

## Stack / Ops

1. **Bearer-token risk boundary** (`server/src/auth.rs`, `deploy/docker-compose.yml`)
   Repo-side audit logging, per-token mutation rate limits, and scoped token
   capabilities now exist. The remaining work is provisioning real non-admin
   tokens in production, moving the live clients onto them where appropriate,
   and deciding whether the primary token should keep full Docker-adjacent
   access long term.

2. **MyDevEnv2 npm registry connectivity for nested app builds**
   In MyDevEnv2, `Working/Active/apps/rustnzbd` currently falls back to the
   placeholder frontend because `npm ci` repeatedly times out against
   `https://registry.npmjs.org/`. Follow up by reproducing under the current
   environment, deciding whether direct npm access is expected to work from the
   pod, and wiring an internal mirror/cache if that connectivity will stay
   constrained. Current reference point: `rustnzb` branch
   `codex/test-uplift-v124`, commit `b52d65d68a1187f3732139afcc422734cb041052`.

---

## Mobile / Android

1. **Fix Android CI release metadata naming**
   The `mobile-apk` step now signs and uploads a real release APK, but pipeline
   `108` published it as `Latest APK (unknown)` / `mydevenv2-.apk` because the
   upload block no longer had the earlier version-name export in scope. Recompute
   or re-export the version metadata in the upload block so Forgejo release
   names and asset filenames stay meaningful.

2. **Real-device native FCM verification**
   `google-services.json` already includes the `com.sprooty.mydevenv2` client;
   the remaining work is confirming first-launch FCM registration and end-to-end
   delivery on actual Android hardware.

---

## Suggested Priority

1. Bearer-token risk boundary
2. MyDevEnv2 npm registry connectivity for nested app builds
3. Android CI release metadata naming
4. Real-device native FCM verification
