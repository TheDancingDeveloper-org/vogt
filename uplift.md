# MyDevEnv2 Outstanding Uplift Backlog

This is the single canonical backlog for remaining uplift work in MyDevEnv2.
Completed uplift work lives in normal repo history, README status notes, and
the code itself. No other markdown file should carry a parallel open-items
backlog.

---

## Stack / Ops

1. **Bearer-token risk boundary** (`server/src/auth.rs`, `deploy/docker-compose.yml`)
   Repo-side audit logging, per-token mutation rate limits, and scoped token
   capabilities now exist. Production now has provisioned read-only and
   interactive scoped tokens, and the live stack is running with
   `MYDEVENV2_EXTRA_TOKENS_JSON` populated. Remaining work is moving the live
   clients onto those scoped tokens where appropriate and deciding whether the
   primary token should keep full Docker-adjacent access long term.

---

## Mobile / Android

1. **Real-device native FCM verification**
   `google-services.json` already includes the `com.sprooty.mydevenv2` client;
   the remaining work is confirming first-launch FCM registration and end-to-end
   delivery on actual Android hardware.

---

## Suggested Priority

1. Bearer-token risk boundary
2. Real-device native FCM verification
