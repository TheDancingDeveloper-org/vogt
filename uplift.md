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
   In MyDevEnv2, `Working/Active/apps/rustnzbd` still falls back to the
   placeholder frontend because `npm ci` repeatedly times out against
   `https://registry.npmjs.org/`. This was revalidated on July 8, 2026 in the
   current MyDevEnv2 pod: package fetches started, then multiple tarball and
   audit requests hit repeated `ETIMEDOUT` failures before the frontend build
   completed. Follow up by deciding whether direct npm access is expected to
   work from the pod and wiring an internal mirror/cache if that connectivity
   will stay constrained. Current reference point: `rustnzb` branch
   `codex/test-uplift-v124`, commit `b52d65d68a1187f3732139afcc422734cb041052`.

---

## Mobile / Android

1. **Fix Android CI release metadata naming**
   Pipeline `109` fixed the Forgejo release title (`Latest APK (0.1.0+b81c086)`)
   and target commit, but the uploaded asset still came through as
   `mydevenv2-.apk`. The remaining work is in the upload block's asset-name
   derivation: make the filename deterministic from the computed version string
   and revalidate the next `mobile-apk` publish end to end.

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
