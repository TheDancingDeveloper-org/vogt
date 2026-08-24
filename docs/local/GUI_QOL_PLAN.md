# GUI quality-of-life delivery plan

Operator-local working document (git-ignored under `docs/local/`). Written
2026-08-22 from the survey tracked in
[#248](https://github.com/TheDancingDeveloper-org/vogt/issues/248). Issues
are the source of truth for *what*; this file is the *order* and the *gates*.

---

## ✅ Handover-items review — the two APK debug items (2026-08-24)

Both debug-APK handover items below were reviewed and closed out this session.

1. **"APK diagnostics — handover brief" (for the next agent)** — EXECUTED IN
   FULL. All six steps done: (1) `VogtApplication` global uncaught-exception
   handler → `filesDir/last-crash.txt` (version via PackageManager, not
   BuildConfig); (2) `MainActivity` copyable monospace crash dialog, read-once;
   (3) `createChannel()`/`acquireWakeLock()` try/catch; (4) version 0.2.0→0.2.1
   (versionCode 2001). Steps 1–4 shipped in **PR #334, merged to `dev`
   (`9283c37`)**, CI green incl. the Android-assembles job. (5) 0.2.1 APK built
   on-host targeting vogt-dev; (6) served at the same URLs
   (`192.168.1.75:18234` / `100.92.54.45:18234`, sha256 `e680e8ae…`).
2. **"⚠ OPEN BUG — Assistant 'unmute' crashes the APK"** — RESOLVED. Operator
   confirmed on 0.2.1 (2026-08-24): no crash, full voice round-trip (speak →
   voice + text reply). #328's foreground-service fix was correct all along; the
   earlier "still crashes" was the STALE build reinstalling over itself
   (versionCode never changed until the 0.2.1 bump — the confounder this brief
   was written to eliminate). The crash recorder now stands as a safety net.

Follow-up raised by the operator: **GitHub #335** (auto-submit the transcribed
utterance on voice-end; note `web/src/Assistant.tsx` already auto-sends at
end-of-take, so likely deploy-lag or a native-path bug — parked, not urgent).

---

## ▶ CURRENT INTENDED STATE — start here (2026-08-23)

### Three parallel streams running (dispatched 2026-08-23, operator-directed)
1. **APK crash diagnostics — ✅ DIAGNOSTIC BUILD DELIVERED (2026-08-24), now
   AWAITING THE OPERATOR'S DEVICE TEST.** The handover brief below was picked
   up and built as a fresh branch `fix/apk-crash-diagnostics` off live `dev`
   `fcb3415` (NOT `fix/voice-crash-diagnostics`, to keep the un-gitignored
   `docs/local/` out of shared `dev`). **PR #334 merged to `dev` (`9283c37`)**,
   CI green incl. the Android-assembles job. Ships: `VogtApplication` global
   uncaught-exception handler → `filesDir/last-crash.txt` (version via
   PackageManager); `MainActivity` shows it once in a copyable monospace dialog
   then deletes it; `createChannel()`/`acquireWakeLock()` try/catch-wrapped;
   version `0.2.0`→`0.2.1` (versionCode 2001). The **0.2.1 dev-targeting APK is
   already served** at the same URLs (`http://192.168.1.75:18234/app-debug.apk`
   LAN / `http://100.92.54.45:18234/app-debug.apk` tailnet, sha256 `e680e8ae…`,
   a superset of the #327 voice APK). NEXT = operator: install → verify Settings
   shows 0.2.1 → Assistant → unmute → reproduce → relaunch → Copy the trace →
   paste back → fix the real cause. The old paused-state note and the
   step-by-step handover brief below are now historical. Note: `docs/local/` is
   TEMPORARILY un-git-ignored (operator, 2026-08-23) — re-ignore before any
   promotion/publish; git history carries it either way.
2. **#292 first-run wizard — ✅ DELIVERED, all three increments merged to
   `dev` (2026-08-23, dev tip `fcb3415`).** PRs #329 (core install-mode),
   #330 (PWA identity wizard + engine `/api/install/*` passthrough), #331
   (`#/setup` steps route; carries `Closes #292` — the issue stays OPEN until
   dev→main promotion, as usual). Details in "#292 — delivered" below.
3. **vogt-prod deploy from dev as-is — ✅ DONE, VERIFIED HEALTHY (2026-08-23).**
   Prod now runs the true dev tip `51a0c66` (waited for its build.yml run to
   finish; the `9670529` build had FAILED at the GHCR base-copy step, so no
   images exist for that sha). Images pinned: core
   `vogt@sha256:81517193…` + stack `vogt-stack@sha256:6c306997…` (both tag
   `dev-51a0c66`). Pre-flight: ops `personal/vogt-prod/` three files
   byte-identical to `deploy/` at `51a0c66` (the 2026-08-23 #205 sync still
   holds; nothing synced). Env: all 50 `${VAR}` refs resolve. Deploy:
   `UpdateStack` (two digest lines only) + explicit `DeployStack` → success,
   `deployed_hash` `9b293c0`→`f9e6396`; no coding sessions were live.
   Verified: `/health/ready` ready (schema 14→**15**, same instance
   `ins_01M0K8YW…`, volume intact), `/version` 0.2.0, PWA `<title>Vogt</title>`,
   both containers healthy at +1 min and +4 min. Rollback digests (old
   `prod-70e7bee` pair) recorded in session scratchpad `prod-rollback.md` —
   **copy them here or to ops notes if the session closes**: core
   `sha256:318804…`, stack `sha256:35081c…` (full digests in Komodo history).
   Deliberately not done: no voice sidecars on prod (assistant stt/tts have no
   backends there; dev's static-IP overlay pattern applies on `10.64.0.0/16`
   if wanted); no dev→main promotion. ⚠ Drift note: prod now runs a `dev-`
   stream image while the `prod` BRANCH is still at `70e7bee` — a future
   prod-branch push would build OLDER code than what is deployed; reconcile at
   the next promotion.

**`dev` = `a809244`, deployed live and healthy on vogt-dev** (Phase-1 PWA + the
`vogt-engine` binary; `/health/ready` → ready). The whole GUI QOL pass (waves
1–4) and this session's follow-ups are on `dev`. PWA sign-in token = the
**engine** token `apps/prod/MYDEVENV2_TOKEN` (the `vogt_*` core/agent tokens 401
at the front door). Base URL for the phone: `https://vogt-dev.sprooty.com`.

### Done (this session)
Phase 1 merged — #208 (#318), #271 (#321), #198 (#323) — plus latent-drift
fixes #319 (ruff), #322 (cargo-fmt), and the product-identity tests reconciled
to #271 (in #323). #324 = **#293 increment-1** (workflow-engine seam, Fabro
provider vs a fake server; live SSE/gates deferred to increment-2). #325 =
**#207** node-b label cleanup (rest of #207 was #315). Deploy recovered after a
2.5 h crash-loop (two-repo overlay drift — see [[vogt-dev-deploy-mechanism]]).
Issue **#326** filed = the first-party Rust voice sidecar (future option "B").

### ⚠ OPEN BUG — Assistant "unmute" crashes the APK (2026-08-23, NOT yet fixed)
Operator testing the dev APK: **Assistant screen → tap unmute (🔇→🔊) → app
crashes.** Unmute (`toggleTts`) primes `speechSynthesis` and, via a
`createEffect`, calls `startVoiceService()` → the native `VoiceConversationService`
foreground service.
- **First hypothesis (PR #328 — ✅ MERGED to dev `51a0c66`, 2026-08-23):**
  Android 14+ `startForeground(..., MICROPHONE)` without `RECORD_AUDIO` throws
  `SecurityException` (uncatchable by the JS try/catch) → crash. Fix: request
  `MICROPHONE` type only when `RECORD_AUDIO` granted, else `DATA_SYNC`; wrap
  `startForeground` in try/catch. Rebuilt the APK (served at the same URL).
  **BUT the operator reports the updated APK STILL crashes** — wrong or
  incomplete. Operator decision (2026-08-23): merge the hardening anyway (it is
  correct on its own), keep the bug open, continue on a fresh branch.
- **Operator decisions (2026-08-23, unblocking both streams):**
  1. **Diagnostics path = blind hardened build** (operator-chosen over
     hands-on adb): new branch off dev with (a) a global
     `Thread.setDefaultUncaughtExceptionHandler` that persists the stack trace
     and SHOWS it on next launch, (b) `acquireWakeLock()` / `createChannel()`
     wrapped in try/catch, (c) **versionCode/versionName bumped** so the
     operator can verify the reinstall actually took (the previous rebuild was
     indistinguishable from the old APK on-device — "still crashes" may have
     been the OLD build). Operator tests when convenient; no adb needed.
  2. Next suspects if the trace doesn't say otherwise: `acquireWakeLock()`
     (outside the old try/catch), `createChannel()`, `speechSynthesis.speak()`
     in the System WebView, or not the FGS at all.
- **Do NOT block anything else on this bug** (operator, 2026-08-23). #292 and
  the rest of the queue proceed in parallel.

### APK diagnostics — handover brief (2026-08-23, for the next agent)
> ✅ **DONE 2026-08-24 — delivered as PR #334 (dev `9283c37`); 0.2.1 APK served. The steps below are historical.**

Operator decision: **blind hardened diagnostic build** (no adb from operator).
Investigation state — READ FIRST, don't redo it:
- `mobile/android/app/src/main/AndroidManifest.xml` is **already complete**:
  `FOREGROUND_SERVICE`, `FOREGROUND_SERVICE_MICROPHONE`,
  `FOREGROUND_SERVICE_DATA_SYNC`, `WAKE_LOCK`, `RECORD_AUDIO` all declared;
  the service declares `foregroundServiceType="microphone|dataSync"`. So a
  missing-manifest-permission theory is DEAD; #328's runtime-grant fix (merged)
  covers the API-34 mic-type case.
- The APK loads the deployed front door via `server.url`
  (`mobile/capacitor.config.ts`) — **PWA JS ships via vogt-dev deploys, NOT
  the APK**. Only native code needs the rebuild.
- Unverified confounder: the operator's "still crashes" test may have run the
  OLD build — versionCode never changed between builds. Hence the bump below.

Work to do on branch `fix/voice-crash-diagnostics` (exists, empty, off
`51a0c66` — dev has since moved to `fcb3415` (#292 wizard merges, no
`mobile/` overlap), so `git rebase origin/dev` first; PR to `dev`):
1. New `VogtApplication` (custom `android.app.Application`, registered via
   `android:name` on `<application>`): `Thread.setDefaultUncaughtExceptionHandler`
   persisting versionName/versionCode (via `PackageManager`, NOT `BuildConfig`
   — buildConfig generation may be off under AGP 8), thread name and full
   stack trace to `filesDir/last-crash.txt`, then delegating to the previous
   handler. This also catches the WebView render-process-gone kill and
   anything from `VoiceConversationService` (same process).
2. `MainActivity.onCreate`: if `last-crash.txt` exists, show an
   (androidx.appcompat) AlertDialog titled with the version, monospace-ish
   trace text, a **Copy** button (clipboard) + Dismiss; delete the file after
   reading. This is how the operator gets us the trace with zero tooling.
3. `VoiceConversationService.startInForeground()`: wrap `createChannel()` and
   `acquireWakeLock()` each in try/catch (log + continue) — they sit outside
   the existing `startForeground` try/catch.
4. Bump `mobile/package.json` version `0.2.0` → `0.2.1` (semver drives
   versionCode `2000` → `2001` in `mobile/android/app/build.gradle`); tell the
   operator to check Settings→Apps→Vogt shows **0.2.1** before testing.
5. Build: `cd mobile && VOGT_ANDROID_SERVER_URL=https://vogt-dev.sprooty.com
   pnpm build:apk` (on-host Android toolchain; `build:apk` = `cap sync android
   && gradlew assembleDebug`; appId stays `com.sprooty.mydevenv2`). Serve at
   the SAME URLs as before via the `vogt-apk-share` container on Node B
   (`http://192.168.1.75:18234/app-debug.apk` / `http://100.92.54.45:18234/…`)
   — replace the file or recreate the throwaway container.
6. Operator loop: install 0.2.1 → verify version → reproduce (Assistant →
   unmute) → relaunch → the crash dialog shows the trace → Copy/paste it back.
   Then fix the real cause on the same branch.
Constraint: the #292 wizard agent owns `web/` right now — do not touch
`web/src` from this stream (JS-side voice fixes deploy via vogt-dev anyway).

### Voice #317, option A — DELIVERED (2026-08-23)
Product PR #327 merged; voice sidecars deployed to vogt-dev and **verified
working end-to-end** (STT+TTS); dev-targeting **APK built** (`app-debug.apk`,
`server.url=https://vogt-dev.sprooty.com`, appId `com.sprooty.mydevenv2`) via the
on-host Android toolchain (`cap sync` + `gradlew assembleDebug`) and served for
install at `http://192.168.1.75:18234/app-debug.apk` (LAN) /
`http://100.92.54.45:18234/app-debug.apk` (tailnet) by a throwaway
`vogt-apk-share` container on Node B (remove with `docker rm -f vogt-apk-share`).
APK install replaces any existing `com.sprooty.mydevenv2` app with a dev-pointing
one. Remaining voice work is the #188–#194 checkpoint validation on the deployed
APK. Details below.

### Voice #317, option A — build log (2026-08-23)
- **Product PR #327 MERGED** (dev `9670529`): `deploy/voice.overlay.yml` (optional
  generic voice overlay: speaches STT + openedai-speech TTS, wired to the engine,
  fully overridable), `theclawbay` hostname default neutralized, docs
  (`USER_GUIDE §2.5`, `CUSTOMISATION`, `.env.example`). **Verified by a real
  docker smoke** — TTS→STT audio round-trip. Working values: both images listen
  on `:8000`; STT model `Systran/faster-whisper-small` (speaches pre-fetches at
  boot — it does NOT auto-download), TTS `tts-1`/`tts-1-hd`, voice `nova`.
- **Dev deploy DONE — voice WORKS end-to-end on vogt-dev (2026-08-23).**
  `/api/assistant/tts` → valid MP3; `/api/assistant/stt` → transcribed text.
  Deployed the `whisper`+`tts` sidecars into the vogt-dev Komodo stack.
  **Gotcha that cost a debugging round (now in [[vogt-dev-deploy-mechanism]]):**
  the engine runs Tailscale, which overwrites `/etc/resolv.conf` with MagicDNS,
  so docker service-name DNS does NOT resolve inside the engine — the estate
  already pins the core via a static IP + `extra_hosts` for exactly this reason.
  The voice sidecars first got dynamic IPs + no pin → "all TTS backends failed /
  connection refused". Fixed by pinning `whisper=10.59.0.11`, `tts=10.59.0.12`
  on the stack subnet (`10.59.0.0/16`) in `ops:personal/vogt-dev/voice.overlay.yml`
  and pointing `MYDEVENV2_ASSISTANT_STT/TTS_BASE_URLS` straight at those IPs
  (no DNS). Models cache in named volumes (`whisper-cache`/`tts-voices`).
  NOTE: the public `deploy/voice.overlay.yml` (#327) correctly uses service names
  — the static-IP treatment is estate-specific (only the Tailscale-in-engine
  stack needs it). **Remaining #317 piece: a dev-targeting APK** (repo var
  `VOGT_ANDROID_SERVER_URL` points at prod, so a stock CI APK targets prod;
  need one built with `VOGT_ANDROID_SERVER_URL=https://vogt-dev.sprooty.com`).

- **Dev deploy STAGED (non-destructive, verified not-deployed):** pushed
  `ops:personal/vogt-dev/voice.overlay.yml` (`e954d66`→`b27116c`) and
  `UpdateStack`'d vogt-dev — added `voice.overlay.yml` to file_paths + set
  `MYDEVENV2_ASSISTANT_STT_BASE_URLS=http://whisper:8000/v1`,
  `_STT_MODEL=Systran/faster-whisper-small`, `_TTS_BASE_URLS=http://tts:8000/v1`,
  `_TTS_MODEL=tts-1`, `_TTS_VOICE=nova`. `deployed_hash` unchanged (`6808e84`),
  secrets intact. **The only remaining action is `DeployStack` — human-gated**
  (consequential: restarts the stack, ~2 GB voice-model download on first start;
  the last vogt-dev redeploy crash-looped 2.5 h). On the operator's go: DeployStack
  → verify `/api/assistant/stt`+`/tts` stop 404-ing → build a dev-targeting APK.

### #292 first-run wizard — GREENLIT, in progress (2026-08-23)
Operator said continue. Security model = the issue's own spec (**zero-tokens
gate**: install-mode active iff no tokens exist; an unauthenticated bootstrap
issues the first browser token, then install-mode self-closes), which is
reasonably safe given Vogt's **loopback-by-default** exposure (`VOGT_BIND_IP`
defaults `127.0.0.1`) — an operator publishes only after setup. Optional
hardening (boot-code / loopback-only bootstrap) = a follow-up, not v1. Building
blocks that already exist to reuse per step: `issue_token`
(`application/services/auth.py`), `actor.create`, `forge onboard`
(`services/writeback.py`), project import (`services/imports.py`); token store
`list_tokens`/`insert_token` (`storage/sqlite/declared.py`) for zero-token
detection. Plan: increment-1 = core (install-mode status endpoint +
unauthenticated bootstrap token issue gated on zero-tokens + auto-close), with
tests; increment-2 = PWA wizard route; increment-3 = forge-link + project steps.

### #292 — DELIVERED (2026-08-23, worktree agent; all three increments merged)
Operator unblocked this stream from the APK bug and approved all three
increments back-to-back; the agent delivered and merged each on green CI.
- **Inc-1 core install-mode — PR #329** (merge `abc18be`):
  `application/services/install.py` (`install_status`, `install_bootstrap`,
  `install_mode_active`), unauthenticated `GET /api/install/status` +
  `POST /api/install/bootstrap` mounted beside the health probes (not
  registry-generated), 409 `install_closed`. Design points: install mode
  counts ALL token rows incl. revoked (revoking the last token never reopens
  the door); the zero-token check runs inside the write txn (racing
  bootstraps: loser rolls back); bootstrap write is audited to the actor it
  creates (`human:<slug>`, admin-scoped non-expiring token); routes live
  under `/api` so the engine's mapping reaches them; hardening slot
  documented in `adapters/http/install.py`. Gates: pytest 1206, cov 91.32%,
  13 new tests; mypy/ruff/check_docs clean; docs (GETTING_STARTED headless
  curl, DESIGN §4.1).
- **Inc-2 PWA wizard + engine passthrough — PR #330** (merge `ee94f92`):
  `web/src/SetupWizard.tsx` (pre-auth wizard at `/`: claim instance → secret
  shown once + copy + CLI/MCP equivalents → hands off to normal sign-in;
  "Sign in instead" escape), `web/src/installApi.ts` (deliberately outside
  `vogtApi.ts`'s registry-checked table), App gate shows the wizard only when
  no saved token (a rejected token never shows it). Engine forwards
  `/api/install/*` OUTSIDE the bearer gate, nothing injected (like `/mcp`),
  routes pinned by a unit test to `INSTALL_PATHS`; ENGINE.md §5. Gates:
  cargo fmt/clippy `--all-targets`/test (44 front-door incl. 2 new), vitest
  713, Playwright 252 collected with 3 new first-run tests green at both
  viewports. (One CI round-trip: clippy `items-after-test-module` — local
  bare clippy vs CI's `--all-targets`; fixed `dc29d39`.)
- **Inc-3 setup steps — PR #331** (merge `fcb3415`, carries `Closes #292`):
  `web/src/SetupSteps.tsx` at `#/setup` INSIDE the authenticated shell
  (steps 2–3 need the front-door credential): forge PAT link via
  `forge.account_link` with as-whom pass/fail + already-linked + skip; first
  project via `forge.repos` picker → `project.import` or name+path →
  `project.register`; then first `sweep` + `coverage` table; every write
  collects a typed reason; all via registry-parity-checked `vogtApi.ts` ops
  (added `project.register`, `forge.account_status`, `forge.account_link`,
  `sweep`, `coverage`). `vogt.setup.pending` flag routes first sign-in to
  `#/setup`, cleared on finish; `/setup` added to `routes.ts` + `routeModel.ts`.
  Gates: vitest 720, Playwright 254 collected, setup walk-through green both
  viewports. ~835 changed web/src lines excl. tests (soft cap ≤800, noted).
- **Deferred (stated on #331):** boot-code/loopback-only bootstrap hardening
  (named slot exists); issue step 4 engine-link confirmation + the
  agent-executable install doc (→ #288); batch import (Projects owns it);
  no `vogt init --wizard=false` needed — provisioning any token (e.g. the
  #199 adopted core token) is the documented headless disable.
- **Playwright host caveat:** the full suite shows the SAME 15 host-specific
  baseline failures on clean `origin/dev` (screenshot/geometry, e.g. 62px vs
  38px headers) — proven identical by name; zero new failures from any
  increment.
- **#292 stays OPEN on GitHub** until dev→main promotion (Closes fires on
  the default branch only).
- ⚠ **vogt-prod is now BEHIND dev again:** prod deployed `51a0c66` (stream 3)
  before these three merges landed (`fcb3415`). If prod should carry the
  wizard too, repeat the digest bump with the `dev-fcb3415` images once
  build.yml publishes them (same procedure, ops overlay diff first).

### NEXT (voice #317 is now DELIVERED — see the DELIVERED block above)
Immediate: operator runs the **#188–#194 voice checkpoint validations** against
the deployed dev APK (`http://192.168.1.75:18234/app-debug.apk`). Then, per the
ordered backlog below, the next agent-actionable feature is **#292 first-run
wizard** (design-sensitive — auth/install-mode; confirm the token-issue/storage
design before building), and the rest is blocked on operator/infra
(#293-inc2 needs a Fabro instance; #295/#297 need CI secret+runner; Phase-4 OSS
is human-ordered; `dev→main` promotion needs the manual phone smoke).

### (superseded) NEXT — voice #317, option A (operator-approved: off-the-shelf sidecar, generic, no lock-in)
The engine is already provider-agnostic (standard OpenAI `/audio/*` interface via
`ENGINE_ASSISTANT_STT/TTS_BASE_URLS`/`_API_KEY`/`_MODEL`). OpenRouter and Gemini
do NOT serve that interface (probed — OpenRouter's `/audio/*` are a different
shape; Gemini-openai-compat 404s), so the generic answer is a self-hostable
OpenAI-compatible pair, shipped by Vogt as an **optional** thing (users bring
nothing mandatory). Deliver in this order:
1. **Product PR** — (a) neutralize the dead `api.theclawbay.com` **chat** default
   at `deploy/vogt-stack.compose.yml:393` (`MYDEVENV2_ASSISTANT_BASE_URL`) →
   estate-neutral (exposure value → no maintainer host); (b) add an optional
   **`voice` compose profile** to `deploy/vogt.compose.yml` (the public base) —
   two profile-gated services: an OpenAI-compatible **Whisper STT** server (e.g.
   `speaches`/`faster-whisper-server`) + an OpenAI-compatible **TTS** server
   (e.g. `openedai-speech` with Piper/Kokoro), pre-wired to the engine over the
   compose network; default stack stays lean; (c) docs (`USER_GUIDE`,
   `.env.example`, `CUSTOMISATION`) — "enable the `voice` profile, or point
   `ENGINE_ASSISTANT_*_BASE_URLS` at any OpenAI-compatible provider."
2. **Deploy the sidecar to vogt-dev** (Komodo) — add the two services to the
   vogt-dev stack (sidecar mirroring the OSS profile) or a small separate stack;
   set `ENGINE_ASSISTANT_STT/TTS_*`; verify `/api/assistant/stt` + `/tts` stop
   404-ing and voice works end-to-end. Reference ports/defaults target
   `127.0.0.1:2022` (STT) / `:8880` (TTS) per voicemode.
3. **Build a dev-targeting APK** (`VOGT_ANDROID_SERVER_URL=https://vogt-dev.sprooty.com`)
   so voice is testable in the actual WebView (the WebView has no Web Speech, so
   the APK depends on server STT/TTS — that's the whole reason #317 gates APK voice).

### Then (ordered backlog — unchanged intent)
- **#293 increment-2** — Fabro live: SSE event mirroring, gate bridging (#289),
  #283/#284 checkpoint-branch collection. Needs a reachable Fabro instance (infra
  decision). Open mapping Q: Fabro human-gate/interrupted → Vogt `Blocked` vs `Failed`.
- **Voice remainders #188–#194** — checkpoints B–D, FCM #191, #192 APK first-run,
  #194 OpenRouter hardening — validate on deployed dev + the APK once #317 lands.
- **#295** e2e stack smoke, **#297** load/soak — need the running stack + a CI
  secret/runner (operator).
- **#292** first-run wizard (web + core install-mode).
- **OSS pre-publish (Phase 4, human-ordered):** #265 Firebase rotate → #271
  Android half → #204 estate-compose fence → #266 package public → repo public;
  close trackers #273/#287/#298/#188.
- **`dev→main` promotion** — after the manual phone smoke passes (Definition of
  done); the promotion PR carries `Closes` for Bucket A (the `0 release` milestone).

### Operator-gated decisions — resolved (2026-08-23)
- **Secret rotation — operator declined** ("ignore secret exposure issue"). The
  transcript-exposed keys (`MYDEVENV2_TOKEN` engine master, Infisical client
  secret, OpenRouter key, Firebase #265, three dev `vogt_*` tokens) are being
  accepted as-is; do NOT rotate unless the operator revisits.
- **vogt-prod overlay sync — DONE** (operator: "execute the vogt prod overlay
  sync"). Synced `indexarr/ops:personal/vogt-prod/estate.overlay.yml` to the
  product `deploy/estate.overlay.yml` #205 shape (ops `6808e84`→`e954d66`);
  additive only, no functional line removed. Verified it did NOT deploy
  (prod `deployed_hash` unchanged `9b293c0`, prod healthy). Prod's next deploy
  will no longer crash-loop on the two-repo drift.

### Fresh-session must-reads
Memories: [[vogt-dev-deploy-mechanism]] (deploy steps + the 4 incident rules +
**GetStack leaks all secrets inline — filter it**), [[gui-qol-agent-loop]]
(**worktree agents can fork a STALE base — always check `git merge-base
<branch> origin/dev` == origin/dev AND the `playwright test --list` count before
trusting a worktree agent's gates**; isolated Playwright is the real gate),
[[oss-readiness-review-2026-08-22]], [[ci-self-hosted-by-design]]. Auth: wrap
git/gh in `mydevenv2-agent-auth run -- …`; push via the
`x-access-token:$GH_TOKEN` credential helper. Latent-drift lesson: a merge that
skips a path-filtered CI job hides formatter/toolchain/identity drift until the
next PR that touches that path — run `ruff format --check` / `cargo fmt --check`
locally before a core/engine PR.

---

## Ground rules

1. **Target branch is `dev`.** Every PR is opened against `dev`; `main` and
   `prod` move by promotion, never by feature PR. Branch names:
   `gui/<pr-slug>`.
2. **Group PRs by surface and shared code**, not by issue. One PR may close
   several issues; one issue may not be split across PRs unless stated below.
   Keep each PR reviewable: target ≤ ~800 changed lines in `web/src`.
3. **Develop locally.** The PWA runs without the engine:
   - `cd web && pnpm install && pnpm dev` — Vite with the mocked API used by
     `tests/browser/gui.spec.ts` (extend the fixtures there, not ad-hoc).
   - Engine-backed checks when a PR touches `engine/`: `cd engine && cargo
     run -p mydevenv2-server` with `MYDEVENV2_TOKEN` set, PWA via `pnpm dev`
     proxied (see `web/vite.config.ts`).
   - Core-backed checks when a PR touches `/api/vogt`: `uv run vogt serve
     --no-auth` on loopback.
   Only use a deployed instance for the final smoke of a merged `dev`.
4. **Every change is covered by tests, including GUI tests.** A PR is not
   ready until all four hold:
   - **Unit** (`cd web && pnpm test`, vitest in `web/src/__tests__/`): every
     new behaviour has a test; every bug fix has a test that failed before.
   - **Browser** (`cd web && pnpm exec playwright test`,
     `web/tests/browser/gui.spec.ts`): every user-visible change has a
     Playwright assertion at **both** 1440×900 and 390×844 when the change
     is layout-bearing; screenshots updated deliberately
     (`--update-snapshots`) and reviewed, never blindly.
   - **Typecheck/lint**: `pnpm typecheck`; engine PRs `cargo clippy
     --workspace --all-targets -- -D warnings` + `cargo test --workspace`;
     core PRs `uv run pytest`, `uv run mypy`, `uv run ruff check .`.
   - **Docs**: `docs/USER_GUIDE.md` updated where behaviour changes
     (several issues exist *because* the guide promised more than was
     built); `uv run python scripts/check_docs.py` green.
5. Each PR body lists the issues it closes (`Closes #nnn`) and names the
   tests that cover each one.

## Test-harness prerequisite (do first)

**PR 0 — `gui/test-fixtures`** · no issue closed, enables everything else
- Extend `web/tests/browser/gui.spec.ts` fixtures to the survey's shape:
  8 work items across 4 states, 4 inbox entries (one drift, one with
  `source_url`, one with neither work item nor session), 3 sessions
  (idle / waiting / busy), history, an assistant transcript with a pending
  approval. Add a `phone` project (390×844) to `playwright.config.ts` so
  every spec runs at both sizes.
- Add a reusable `mockApi()` helper so later PRs add one route, not a copy.
- Gate: existing specs green at both sizes; snapshot baselines committed.

## Delivery order

Ordered by daily cost, with each PR's issues, code area, and explicit test
gate. Severity in brackets from the survey.

### Wave 1 — phone is usable

**PR 1 — `gui/phone-sessions-shell`** · closes #232, #233
- `web/src/Sessions.tsx`, `styles.css` (terminal-layout, surface header at
  ≤768px), a running-sessions list shared by the desktop overview.
- Gate: Playwright at 390×844 asserts `.terminal-host` height ≥ 40dvh on
  `/t/:id` and first useful content top < 400px on History/Git/Assistant;
  desktop overview lists 3 sessions and shows "Start a session" when empty.

**PR 2 — `gui/phone-navigation`** · closes #231, and the phone half of #245
- `App.tsx` bottom bar ("More" sheet → remaining places, Settings, Sign
  out), `--keyboard-inset` consumed by the bar and the palette, rail keyboard
  toggle (Ctrl/Cmd+B).
- Gate: Playwright phone reaches every place and Settings/Sign out in ≤ 2
  taps from any surface; desktop `Ctrl+B` toggles the rail; unit tests for
  the sheet's place list.

**PR 3 — `gui/inbox-phone-sheet`** · closes #219 (small, ship alone — it is
a one-rule CSS bug)
- Gate: Playwright phone screenshot of the opened action sheet; assertion
  that the backdrop is `position: fixed` and the entry text width is
  unchanged while open.

**PR 4 — `gui/board-touch-move`** · closes #214, #216
- `Board.tsx` "Move…" control on coarse pointers reusing `beginMove` /
  `commitMove`; lane-drop hint.
- Gate: unit test for the composer path; Playwright phone moves a card
  between states without drag; desktop same-column lane drop shows the
  hint and makes no request.

### Wave 2 — work surfaces are honest and complete

**PR 5 — `gui/work-item-editing`** · closes #213, #224
- `WorkItemDetail.tsx`: Move-to + reason via `transitionWork`; Edit panel
  extended to assignee/effort/labels/body via `updateWork`; session form
  collapsed; prose trimmed. `USER_GUIDE.md` §4 corrected.
- Gate: unit tests for optimistic transition + rollback on 409; Playwright
  desktop + phone changes state from the page; a11y check that the actor
  picker is keyboard-operable.

**PR 6 — `gui/filter-persistence`** · closes #215
- `App.tsx` `rememberPlace` carries last query; Board/Backlog seed from it.
- Gate: unit test on the seeding; Playwright: filter → open item → rail →
  filter still applied (both surfaces).

**PR 7 — `gui/inbox-live-and-layout`** · closes #218, #220, #221
- `Inbox.tsx`: `onVogtLive` with composer guard, merge by `entry_key`,
  pagination preserved; evidence collapsed; sticky batch bar; header via
  SurfaceHeader; `source_url` link; dead "Open entry" hidden; shortcut
  legend; `ViewAgeBadge`.
- Gate: unit test that a nudge during composing does not reset the
  composer and keeps page 2; Playwright desktop shows ≥ 4 entries per
  screen and the Source select is not clipped; phone screenshot.

**PR 8 — `gui/reconcile-and-markdown`** · closes #223, #222, #225
- `WorkItemDetail.tsx`/`Backlog.tsx` subscribe with a composer guard;
  sanitising markdown renderer for bodies/comments/cards (unit-tested
  against script injection); History search debounce.
- Gate: unit tests for the renderer's allow-list and for the debounce
  (one request per settled query); Playwright renders a fixture body with
  a heading, list and fenced code.

**PR 9 — `gui/names-and-links`** · closes #217, #227
- Actor/project name resolution with raw ref in `title`; project ↔
  board/backlog/audit links; Projects list search/sort.
- Gate: unit tests for the resolver fallback; Playwright follows a project
  link and lands with `?project=` applied.

**PR 10 — `gui/backlog-rows`** · closes #226
- Gate: Playwright phone shows ≥ 3 rows on first screen; chip text reads
  "this page only"; bulk bar sticky assertion.

**PR 11 — `gui/board-chrome`** · closes #229
- Gate: Playwright `.board-card` top < 200px desktop / < 300px phone;
  confirm-on-overwrite unit test; card opens on Space.

### Wave 3 — sessions, editor, git, tasks, assistant

**PR 12 — `gui/split-compose`** · closes #212
- `TerminalWorkspace.tsx`, `workspaceLayouts.ts`, palette actions.
- Gate: unit tests for insert-existing / swap / re-target / detach with
  zero `createSession` calls; Playwright builds a split from two existing
  sessions and closes a pane without killing it (assert no DELETE).

**PR 13 — `gui/terminal-search-and-state`** · closes #234, #235
- `@xterm/addon-search`; find bar; reconnect overlay with retry/count/
  queued bytes; exited-shell banner; font-size readout; cwd-based naming.
- Gate: unit tests for the reconnect state machine and the naming rule;
  Playwright: search highlights a fixture line; exited fixture shows the
  banner and "Remove" skips the confirm.

**PR 14 — `gui/phone-terminal-input`** · closes #236
- Gate: unit test that armed Ctrl + printable sends the control byte once
  then clears; Playwright phone toolbar fits without horizontal scroll and
  the error span is visible; copy chip appears after a selection.

**PR 15 — `gui/editor-integrity`** · closes #237, #238, #239 (engine touch:
`readFile` returns mtime/hash; `writeFile` accepts `if_match`)
- Gate: `cargo test` for the conflict path (412/409); unit tests for the
  on-disk-changed banner and draft-vs-disk notice; tree state store tests;
  Playwright: edit → external change via mock → Save shows Overwrite/Reload;
  tab switch keeps expansion; Git stage-all then commit enabled.

**PR 16 — `gui/phone-editor`** · closes #240
- Gate: Playwright phone editor width ≥ 70% of viewport with the drawer
  closed; splitter responds to touch pointer events.

**PR 17 — `gui/agent-tasks`** · closes #241
- Gate: unit tests for the two new fields and findings rendering;
  Playwright: Run Now while dirty is "Save & Run"; reload keeps the draft.

**PR 18 — `gui/assistant-ergonomics`** · closes #242 (engine touch: accept
request abort cleanly)
- Gate: unit tests for failed-send restore + retry and AbortSignal wiring;
  Playwright: failed send keeps the draft; Stop cancels; Shift+Enter inserts
  a newline; markdown reply renders a code block.

### Wave 4 — shell, palette, settings

**PR 19 — `gui/palette`** · closes #230
- Gate: unit tests for the scorer (exact > prefix > subsequence, label >
  description) with the "needs" → `needs-attention` case; Playwright lists
  Open Inbox/Sessions/History and shows shortcuts.

**PR 20 — `gui/surface-header`** · closes #228, desktop half of #245, #246
- Gate: Playwright screenshots of all six surface headers at both sizes;
  Recent places has no duplicate labels; rail footer visible at 900px;
  empty Board shows one panel with Quick create.

**PR 21 — `gui/settings-and-push`** · closes #243, #244
- Gate: Playwright dialog footer visible at 900px and on phone; destructive
  actions open the confirm; denied permission shows the blocked state;
  unit test for subscription reconciliation against the server list.

**PR 22 — `gui/polish`** · closes #247 (may be split if > 800 lines)
- Gate: one unit or Playwright assertion per bullet in #247.

### Parallel, non-GUI (separate owners/PRs, same `dev` target)

- #198 dropped-connection handling — fold into PR 13 if the same state
  machine, otherwise its own PR.
- #203 `ENGINE_` env names — **done** (PR #278).
- #209 `test_front_door` fix (core) — **done** (PR #252).
- Open-source delivery — now a tracked programme under **#273**, not a loose
  list; see "Open-source readiness" below for how it touches this plan.

## Definition of done per wave

A wave is done when every PR in it is merged to `dev`, `dev` CI is green
(including the Playwright job at both viewports), and a manual smoke on a
phone against the deployed `dev` instance confirms the wave's headline
claims (the ones listed as Playwright assertions above). Then promote
`dev` → `main`.

## Scoreboard

| Wave | PRs | Issues closed | Status |
|---|---|---|---|
| 0 | PR 0 | — | folded into per-PR fixtures; `installFixtures` is the shared helper, phone project already existed |
| 1 | PR 1–4 | #214 #216 #219 #231 #232 #233 (+ part #245) | **DONE** — all merged to dev |
| 2 | PR 5–11 | #213 #215 #217 #218 #220–#227 #229 | **DONE** — all merged (PR 5–11) |
| 3 | PR 12–18 | #212 #234–#242 | **DONE** — PR 12–18 all merged (#212 #234–#242) |
| 4 | PR 19–22 | #228 #230 #243–#247 (+ rest of #245) | **DONE** — PR 19–22 all merged (#228 #230 #243 #244 #245 #246 #247) |

### OSS-readiness delivery (after the GUI plan, per the "Full OSS incl. rename" decision)

- **#282 `engine/oss-clean-rename` — MERGED** (#269, #206): engine renamed **`mydevenv2-server`→`vogt-engine`** (crates `vogt-engine-{server,contract}`, binary `vogt-engine`; `MYDEVENV2_*` env aliases kept). Clean-clone buildable: `build.rs` writes a `web/dist` placeholder; `CORE_IMAGE` default `ghcr.io/thedancingdeveloper-org/vogt:latest`; CORS default estate-neutral (`localhost:5173`) with prod origins moved to `ENGINE_ALLOWED_ORIGINS` in `estate.overlay.yml`; `workspace_root` non-fatal fallback. Deferred: Docker image/tag + `sprooty` user/paths (→ #204, estate-load-bearing). Gates: cargo test 261, clippy, pytest 1099. **Deployed pods pick up the new binary name only at the next build+deploy (human-gated).**
- **#202 `deploy/public-engine-overlay` — in flight**: additive generic `deploy/engine.overlay.yml` + `.env.example` (estate files untouched).

**Operator-coupled — NOT auto-done (need your hand):**
- **#204** relocate the live estate compose (`estate.overlay.yml` etc.) to `docs/local/` — your Komodo reads `deploy/estate.overlay.yml` by path, so `git rm` would break the running deploy; you choose where they live + repoint Komodo.
- **#271 Android half** (package/app-id rename) — ties to #265 (rotate the committed Firebase key — human) and #266 (make the GHCR package public — human, last).
- **#272** delete the legacy `/ui` GUI — only after the stack quickstart exists.
- **`dev→main` promotion** — needs the manual phone smoke of deployed `dev`.

**★ ALL FOUR GUI WAVES DELIVERED TO `dev` (tip 62c24bd), every merge isolated-gated green (typecheck, 656 unit, Playwright 194 collected, `test_pwa.py` 24/24, engine cargo where touched).** 36 GUI issues (#212–#247 range) + engine #203 + core #209, in 17 PRs across 8 rounds. The GitHub issues stay OPEN because `Closes #nnn` fires only on merge to the DEFAULT branch (`main`); everything is on `dev`, awaiting the `dev→main` promotion — which per "Definition of done" needs a **manual phone smoke of the deployed `dev`** first (human/deploy step). Final GUI PR: #281 (#247 polish + Board accessible-confirm a11y fix; `test_pwa.py` now green on dev).

Docs branch **PR #211 merged to dev** (docs pruning, −19k lines; USER_GUIDE conflicts resolved toward #211 keeping dev's non-conflicting additions; `docs/local/` git-ignored again). Push auth: classic PAT needs Basic (`Authorization: Basic base64(x-access-token:PAT)`), not bearer — see [[gui-qol-agent-loop]].

**Rounds E–G merged (`dev` tip 734a08c). All GUI waves 1–4 delivered except the final polish PR #247 (in flight).** Cumulative: **36 GUI issues + core #209 + engine #203**.
- Round E: #263 (#236), #264 (#243 #244), #274 (#237–#239 engine+web)
- #211 docs pruning
- Round F: #275 palette (#230), #276 phone-editor (#240), #277 assistant (#242, engine)
- Round G: #278 engine env (#203, + VOGT_NOTIFY), #279 split-compose (#212), #280 SurfaceHeader (#228 #246 desktop-#245)
- **Gate lesson added** ([[gui-qol-agent-loop]]): a bad `gui.spec.ts` conflict resolution can make Playwright collect **0 tests and exit 0** (false pass) — always `--list` after resolving it; and web rounds must run `uv run pytest tests/test_pwa.py` (Board's `window.confirm` from PR 11 sat red there until Round G; PR 22 fixes it).

### Delivery log (agent loop, 2026-08-22)

Merged to `dev` (each merge re-verified by the full Playwright suite run **in
isolation** — concurrent Chromium suites flake on this box, so the isolated
integration run is the real gate, not the per-agent run):

- PR #249 `gui/inbox-phone-sheet` — #219 (backdropClass replaced modal-backdrop; made the sheet backdrop self-contained)
- PR #250 `gui/board-touch-move` — #214, #216 (coarse-pointer Move… reusing beginMove/commitMove; same-lane drop hint)
- PR #251 `gui/phone-sessions-shell` — #232, #233 (compact phone header, terminal min-height:40dvh, desktop overview + new `SessionList`/`sessionRowModel`)
- PR #252 `fix/front-door-linked-project` — #209 (switched the audited-write test from work.create to label.create; full `uv run pytest` green, 1104 passed)

Integrated `dev` at push: typecheck clean, 465 unit, 73 Playwright passed / 31 skipped.

Batch B merged to `dev` (isolated suite re-verified green: 82 passed / 35 skipped / exit 0; 492 unit):

- PR #253 `gui/phone-navigation` — #231, phone half of #245 (More sheet, `--keyboard-inset`, Ctrl+B rail toggle)
- PR #254 `gui/work-item-editing` — #213, #224 (Move-to + reason, editable assignee/effort/labels/body, collapsed session form, Comments under Description)
- PR #255 `gui/inbox-live-and-layout` — #218, #220, #221 (onVogtLive merge-by-entry_key, collapsed evidence, sticky batch bar, source links, keys legend, ViewAgeBadge)
- PR #256 `gui/board-chrome` — #229 (legend→? dialog, lenses into +Filter, ack in composer cell, card opens on Space/click, Finished-hidden chip, confirm-on-overwrite + undo)

Confirmed operationally: every parallel agent reports the same ~9 desktop + 3 phone "pre-existing" failures, but a **solo** `CI=1 pnpm exec playwright test` on the integrated tree is exit 0 — the failures are Chromium-suite contention on this box, not real. The isolated integration run is the only trustworthy gate.

Round C merged to `dev` (isolated suite green: 90 passed / 37 skipped / exit 0; 515 unit):

- PR #257 `gui/filter-persistence` — #215 (rail/palette/bottom-bar links carry the surface's last query via `surfaceHref`/`rememberPlace`)
- PR #258 `gui/reconcile-and-markdown` — #222, #223, #225 (hand-rolled sanitising `markdown.tsx` — node-building, no innerHTML, entity-disguised `javascript:` blocked; `onVogtLive` reconcile on work item/backlog; 250ms History search debounce)
- PR #259 `gui/backlog-rows` — #226 (State chip "· this page only", "N of M loaded rows", Start-a-session on collapsed row, sticky bulk bar, title-first phone rows, selection stability)

Round D merged to `dev` (isolated suite green: 100 passed / 39 skipped / exit 0; 549 unit; dev tip **800247a**):

- PR #260 `gui/agent-tasks` — #241 (vogt_project/work_item bindings + findings, self-refresh on session-killed, Save & Run when dirty, sessionStorage drafts, UTC label)
- PR #261 `gui/names-and-links` — #217, #227 (shared `refNames.ts` resolver, project↔board/backlog/audit cross-links, `projectRegistry.ts` search/sort; also fixed a latent Board assignee-filter bug)
- PR #262 `gui/terminal-search-and-state` — #234, #235 (`@xterm/addon-search` find bar Ctrl/Cmd+Shift+F, interactive reconnect overlay, exited banner, font readout, cwd naming)

## Open-source readiness — alignment (2026-08-22)

Decision record lives in memory [[oss-readiness-review-2026-08-22]]; the
short form that governs this plan:

- **The OSS product is core + engine + PWA, one two-container stack**
  (tracking **#273**; retires `opensource.md` decision 2 / r20). Trigger: the
  core-alone quickstart served `src/vogt/gui/static/` (719 lines, 7 views) —
  the operator confirmed via `/ui-legacy` on vogt-dev they had never seen it.
  The PWA this plan delivered *is* the public product surface.
- **Repo is PRIVATE again** (operator, 2026-08-22); `vogt` GHCR package was
  already private. #265 (Firebase key in history — still rotate) and #266
  (package visibility) are pre-publish gates, not live exposures. Flipping
  public is the last step and a human decision.
- **CI stays on self-hosted runners by design** (memory
  [[ci-self-hosted-by-design]]). #207/#270 mean "say so in CONTRIBUTING and
  fail soft without the LAN buildcache", never `ubuntu-latest`.
- `USER_GUIDE.md` is the public guide for the stack — keep editing it per PR.
  README/`opensource.md`/DESIGN/ENGINE still describe core-first; that reframe
  is one PR after #300 (queue item 12), not per-feature edits.
- New `localStorage`/event keys use the `vogt.` prefix (#271); engine PRs add
  no `MYDEVENV2_*` names or estate defaults (#269 done — keep it so).

**Issue register** (all under #273 unless noted):

| Issue | What | State |
|---|---|---|
| #273 | Tracking: boundary decision, checklist, acceptance | open |
| #265 | Rotate/remove committed Firebase key | human |
| #266 | `vogt` GHCR package private → default compose can't pull | last |
| #267 | Compose first-run docs (token actor `local:vogt`, `exec` form, host mount, curl wait) | open; #292 wizard is the superset |
| #268 | Tests assert estate constants | open |
| #269 | Engine buildable from clean clone | **done** PR #282 |
| #270 | SECURITY.md, CoC, templates, CHANGELOG, Releases, CONTRIBUTING CI note | open |
| #271 | MyDevEnv2 identity on shipped surfaces (web/mobile/storage keys) | engine half done; Android half ties to #265 |
| #272 | Public path serves legacy GUI → deliver PWA, delete `src/vogt/gui` | after #300 |
| #202 | Public engine compose overlay | **PR #300 open** |
| #203 | `ENGINE_` env names | **done** PR #278 |
| #204 | Estate compose files fence | **operator-coupled**: Komodo reads `deploy/estate.overlay.yml` by path; relocating breaks the live deploy — operator chooses destination + repoints Komodo |
| #205 | Estate defaults in engine scripts | open |
| #206 | Image user/paths | **done** PR #282 (Docker `sprooty` user deferred → #204) |
| #207 | CI comment/label cleanup | open (self-hosted stays) |
| #211 | Docs pruning PR | **merged** |

Other tracks opened the same day: **#287** git integration (#283–#286),
execution **#289–#293**, test strategy **#298** (#294–#297), agent guide
**#288**, app themes **#299**. All placed in the queue below.

## ✅ GUI QOL PLAN COMPLETE — 2026-08-22 (evening)

**`dev` = origin/dev = `ebee2c8`.** Every wave is merged:

- Round E: PR #263 (#236), #264 (#243 #244), #274 (#237 #238 #239)
- Round F: PR #275 (#230), #276 (#240), #277 (#242), #279 (#212), #280 (#228,
  desktop #245, #246), #281 (#247)
- Alongside: PR #278 (#203 `ENGINE_` names), PR #282 (#269 clean-clone build +
  #206 image user/paths; crates are now `vogt-engine-server` /
  `vogt-engine-contract`, binary `vogt-engine`), PR #211 (docs pruning:
  REQUIREMENTS/MERGE_MYDEVENV2/RESTRUCTURE/parity docs gone from the tree,
  `docs/local/` git-ignored)

**Housekeeping owed:** closing keywords fire only on merges to `main`, so the
delivered issues are still OPEN on GitHub — #209, #212–#247 (all), #203,
#206, #269. The `dev→main` promotion PR will not carry the original
`Closes` lines, so either close them by hand with a "merged in PR #nnn"
comment now, or list them as `Closes #…` in the promotion PR body. Either
way, before the next round so the backlog is honest.

**Ground rule 3 is now:** `cd engine && cargo run -p vogt-engine-server` with
`ENGINE_TOKEN` set (the `MYDEVENV2_*` names are a warned fallback only).
`cargo build` works from a clean clone without `web/dist`.

**Open PR:** #300 `deploy/public-engine-overlay` (#202) — the two-container
public stack. Mergeable. This is the gate for the public quickstart.

## Queue — current order (operator-set 2026-08-22)

One list, top is next. GUI work keeps the same rules, gates and batching as
the waves above (distinct primary `.tsx` per agent, isolated Playwright as
the real gate).

| # | Item | Track | Why here |
|---|---|---|---|
| 1 | **#299 app themes** — light / dim / high-contrast beside the kept dark; `System` option; picker in Settings; no flash on load | GUI | **operator pushed to top.** Token sweep touches `styles.css` + ~13 tsx; land it *before* any further GUI PR so later snapshots are taken per theme once, not re-baselined twice |
| 2 | Close delivered issues (see housekeeping) | hygiene | 10 min, makes every later count honest |
| 3 | **Merge PR #300** (#202 engine overlay) | OSS | unblocks the stack quickstart, #295 and #272 |
| 3b | **`dev→main` promotion** — manual phone smoke of deployed `dev` first (Definition of done), then promote; deployed pods pick up the `vogt-engine` binary name only on the next build+deploy | release / human | the waves are not "done" until `main` has them |
| 4 | #265 rotate Firebase key | OSS / human | independent; pre-publish gate |
| 5 | #267 first-run docs (+ #292 wizard if picked up with it) | OSS | a stranger's first 10 minutes; needs #300 to document the stack form |
| 6 | #294 fixture forge repo + #296 synthetic agent CLI | test | both unblock everything below; no product dependency |
| 7 | #283 branch binding + #284 PR edge (parallel) | git (#287) | first git-loop slice; tests need #294/#296 |
| 8 | #295 end-to-end stack smoke + Playwright `live` | test | needs #300 + #294 + #296; then it is #273's acceptance criterion, mechanised |
| 9 | #289 gates/steer, #291 outcomes (parallel; both engine `agent_tasks.rs` + PWA task view) | execution | needs #296 to test; #291 feeds #285 |
| 10 | #285 git story on the work item | git (#287) | needs #283 #284 #291 |
| 11 | #290 event triggers | execution | needs #291 outcomes and a core→engine event subscription |
| 12 | #272 delete legacy GUI + docs reframe (README/opensource/DESIGN/ENGINE → stack) | OSS | only after #300 is the documented quickstart; one PR |
| 13 | #268 estate constants in tests, #205 estate script defaults, #207/#270 contributor docs, #271 Android half | OSS hygiene | before flipping public |
| 13b | #204 estate compose fence | OSS / **operator** | Komodo reads `deploy/estate.overlay.yml` by path — operator picks the new home and repoints Komodo; agents must not `git rm` it |
| 14 | #286 initiative → tracking issue, #288 agent guide (sections 3–4 after #283/#284), #293 external engine backend, #297 load/soak | later | #288 sections 1, 2, 5, 6 can be written any time |
| 15 | #266 packages public; repo → PUBLIC | OSS / human | last; #273 closes |

Not in the queue: #188–#194 voice (separate track, unchanged), #198
dropped-connection in data views (fold into whichever GUI PR next touches
the fetch layer), #200/#201 (recorded branches, no action), #208.

### Ordering notes
- #299 before more GUI: a theme sweep re-baselines every Playwright
  snapshot; doing it once, first, is cheaper than doing it after each later
  PR. It also has zero dependency on anything else.
- Test infrastructure (#294/#296) sits *above* the feature work it tests
  (#283+, #289+). That is deliberate: the git loop and execution features are
  the first work in this repo with no in-process way to test them.
- #300 before #295/#272/#267's stack form: the two-container compose is what
  every public-path claim is made against.
- Nothing in this queue moves the repo or packages public; that is step 15
  and a human decision.

## Resume procedure (current)

1. Token: `HOMELAB_VOGT_GITHUB_TOKEN` in Infisical `apps/prod` (shell
   `GH_TOKEN` is stale). See [[gui-qol-agent-loop]].
2. `git pull --ff-only origin dev`; confirm tip ≥ `ebee2c8`. `git worktree
   prune` — the Round E/F worktrees under `.claude/worktrees/` are done.
3. Close the delivered issues (housekeeping above).
4. Dispatch #299 as `gui/app-themes` (one agent; it owns `styles.css`, so
   nothing else in `web/` runs alongside it). Gate: isolated
   `CI=1 pnpm exec playwright test` exit 0 with per-theme baselines reviewed,
   vitest for the theme store, contrast assertion on Light.
5. Merge #300 in parallel (deploy-only; no `web/` overlap).
6. Continue down the queue, batching by track: one engine (`cargo`) build per
   round; test-infra items (#294/#296) are core/scripts and can share a round
   with anything.

Delivery-mechanics detail is in memory [[gui-qol-agent-loop]]; the
open-source decision record is in memory [[oss-readiness-review-2026-08-22]];
the execution-engine comparison behind #289–#293 is
`docs/local/FABRO_COMPARISON.md` (never reference it outside `docs/local/`).

## Queue v2 — every open issue, one stream (2026-08-23)

Audit of the 72 open issues against `origin/dev` (tip `d47cc06`). Same
ordering principles as the waves: cheapest-daily-cost first, test infra above
the features it tests, nothing goes public until last. The previous queue
(items 1–15) is folded in; nothing is dropped.

### Bucket A — delivered on `dev`, open only because `Closes` fires on `main` (57)

#202 #203 #205 #206 #209 #212–#247 (all 36) #267 #268 #269 #270 #272 #283
#284 #285 #286 #288 #289 #290 #291 #294 #296 #299, plus tracking #248.
Partial and staying open: #207 (runner-label/policy restructure needs the
runners' labels), #273/#287/#298 (tracking, close with their last child).

### The stream

| Phase | Item | Track | Gate / who |
|---|---|---|---|
| **0 — release what exists** | Redeploy `dev` (new PWA, `vogt-engine` binary) | deploy | operator (Komodo) |
| 0 | Manual phone smoke of deployed `dev` (Definition of done) | release | **human** |
| 0 | `dev→main` promotion PR carrying `Closes` for all of Bucket A | release | after smoke |
| 0 | Close #200/#201 as "recorded branch, no action"; close #248 | hygiene | with promotion |
| **1 — small honest fixes** (one hygiene round, no deploy dependency) | #208 `public_url` example + CONFIG.md §4.1 pointer (fix in `config.py`, regenerate) | OSS | docs check green |
| 1 | #271 **web half** — 7 `mydevenv2:*`/`mydevenv2.*` storage keys remain in `web/src` (`auth`, `token`, `terminal-theme`, `terminal-font-size`, `viewport-resize`, `native-insets`, `assistant.tts`); rename to `vogt.*` with a one-shot migration so nobody is logged out | OSS | unit test for the migration; isolated Playwright |
| 1 | #198 dropped-connection retry in the fetch layer — no GUI PR is pending any more, so it gets its own PR | GUI | unit test: TypeError → retryable view error |
| 1 | #207 remainder — needs the self-hosted runners' labels from the operator, then one CI PR | CI | operator supplies labels |
| **2 — validate on the deployed `dev`** | Voice Track 1: #189 desktop mic, #192 APK first-run, #194 OpenRouter end-to-end + key hardening; #191 close if the console verification (aacd0f1) holds | voice | manual, phone + desktop |
| 2 | #295 e2e stack smoke — fixture (#294) + fake agent (#296) exist; needs a CI secret + runner + mock-agnostic `gui.spec` | test | operator: secret + runner |
| 2 | #297 load/soak generator with recorded numbers | test | needs the running stack |
| **3 — features** | #292 first-run wizard (superset of #267, which is done) | OSS | test per step |
| 3 | #293 external workflow-engine backend | execution | after #289/#291 (done) |
| 3 | #317 server-side STT/TTS — provider choice, Infisical key, `ENGINE_ASSISTANT_*` on dev, fix the `api.theclawbay.com` compose default | voice | operator: key |
| **4 — pre-publish, human** | #265 rotate Firebase key → #271 Android half → #204 relocate estate compose + repoint Komodo → #266 package public → repo public; close #273 #287 #298 #188 | OSS | **human**, in that order |

### Making it visible in GitHub

Create four milestones (`0 release`, `1 hygiene`, `2 validate`, `3 features`,
`4 publish`) and assign every open issue; Vogt's forge import then shows the
same order in the backlog without a second document. Bucket A gets
`0 release` so the promotion PR's `Closes` list can be generated from the
milestone instead of by hand.

**Applied 2026-08-23:** milestones `0 release` (60 — Bucket A + #200/#201 to
close as recorded + #248), `1 hygiene` (4), `2 validate` (6: #189 #191 #192
#194 #295 #297), `3 features` (4: #190 #292 #293 #317), `4 publish` (7: #188
#204 #265 #266 #273 #287 #298). All 72 open issues assigned. Whether the
deployed vogt-dev's forge import surfaces milestones is unverified — it still
runs the pre-waves image; check after the Phase 0 redeploy. To generate the
promotion PR body:
`gh issue list -m "0 release" -s open -L 100 --json number -q '.[] | "Closes #\(.number)"'`
(drop #200/#201/#248 from that list — they close by hand).

### Phase 1 delivery log (agent loop, 2026-08-23)

Phase 0 is entirely operator/human-gated (Komodo redeploy → manual phone smoke
→ `dev→main` promotion), so this round works **Phase 1** — the "small honest
fixes" with no deploy dependency. Order within the round: #208 (core, isolated)
+ #271 (web, owns all of `web/src`) in parallel, then #198 (web fetch layer)
after #271 merges, because #198 and #271 both centre on `web/src/api.ts` and
would collide.

**STATUS (dev tip `8533c5c`):** Phase 1 agent-work COMPLETE. #208 ✅ MERGED
(#318), #271 ✅ MERGED (#321), #198 ✅ MERGED (#323), plus THREE latent-drift/
skipped-gate hygiene fixes MERGED — ruff-format (#319), cargo-fmt (#322), and the
product-identity tests reconciled to #271 (folded into #323). #207 remains the
only Phase-1 item open — BLOCKED on operator-supplied self-hosted runner labels.

**Structural finding worth the operator's eye:** CI's per-job path filters mean a
merge that doesn't touch a job's paths SKIPS it, so formatter/toolchain/identity
drift accumulates invisibly until the next PR that *does* touch those paths pays
for it. This session surfaced three such latent breakages in a row (ruff on a
docs-only tip, cargo-fmt from a rustfmt bump, and #271's Python identity tests
that its web-only CI never ran). Consider a periodic full-matrix CI run on `dev`
(nightly or on every merge to `dev`) so drift is caught at its source, not by the
next unrelated contributor.

- **PR #318 `fix/config-public-url-example` — #208** (MERGED): neutral
  `public_url` example (`https://vogt.example.com`, was the retired
  `host.tailnet.ts.net:18094`) + default-policy pointer corrected to
  `DEPLOYMENT.md §4 (Configuration)` (§4.1 is now "Tokens"). Fixed at source in
  `config.py`, regenerated `docs/CONFIG.md` + `config.example.toml`. Gates:
  `check_docs` 46 files green, ruff clean, 55 config/doc tests pass (incl. the
  generated-doc-drift guard).
- **#271 web half — PR #321 `gui/identity-rename-web-v2`** (MERGED; CI incl. the
  self-hosted PWA-embed Playwright with real baselines was green, confirming the
  15 sandbox failures were environmental).
  **The plan's "7 keys" was stale — the real scope is the full ~28-key sweep**
  of every still-current `mydevenv2.*` localStorage key and `mydevenv2:*`
  event/BroadcastChannel name in `web/src`, renamed to `vogt.*`/`vogt:*` with a
  one-shot, sentinel-guarded migration (`web/src/storageMigration.ts`, explicit
  old→new map) so nobody is logged out and no prefs are lost. Deliberately kept:
  the #299 legacy read-fallback keys (`appTheme.v1`, `commandPalette.recent.v1`,
  `fileTree.sidebarCollapsed.v1`), `tabs.v1`, the Android channel id
  `mydevenv2-alerts`, IndexedDB `mydevenv2-terminal-cache`, and the
  `mydevenv2:native-insets` event (web now dual-listens `vogt:`+`mydevenv2:`).
  Android app-id half stays human-gated (#265). Gates: typecheck clean, vitest
  697 (new `storageMigration.test.ts`, 8 cases), Playwright 246 collected /
  180 passed / 15 pre-existing-only failed (proven identical to clean `dev`),
  `test_pwa.py` 22.

  **⚠ WORKTREE STALE-BASE BUG (new lesson).** The first dispatch used an
  `isolation: worktree` agent; the worktree forked from **`70e7bee` — 93 commits
  behind `dev`**, not live HEAD. The agent's rename therefore missed every key
  added after the fork (`appTheme.v1`, `backlog.poll.v1`, `fileTree.expanded.v1`),
  and its "isolated Playwright, no regressions" check ran against a suite that
  **collected only 98 of the real 246 tests** — the exact collection-drop
  false-pass the plan already warns about, here caused by a stale base rather
  than a broken spec. Its PR **#320 was closed**; the work was redone by hand on
  a branch correctly based on current `dev` (#321), reusing the agent's migration
  design. **Rule added: after any worktree agent, `git merge-base <branch> dev`
  and `playwright test --list` count MUST be checked before trusting its gates —
  `isolation: worktree` here does NOT fork from live HEAD.**
- **#198 — PR #323 `gui/dropped-connection-retry`** (OPEN, CI running). New
  `web/src/transport.ts` `fetchWithRetry` behind both fetch choke points
  (`vogtApi.ts` `call()`, `api.ts` `req()`): GET/HEAD retry twice with jittered
  backoff, POST never retries (double-write guard), AbortError/aborted-signal
  pass through (SSE + cancel intact), HTTP errors resolve untouched (VogtUnavailable
  path unchanged); on exhaustion a typed `TransportError` with a written reason
  that `serverReason` already surfaces. Updated `test_pwa.py`'s "one call site"
  invariant to the new choke point + a no-bare-`fetch` guard. Gates: typecheck,
  vitest 704 (new `transport.test.ts`, 7 cases), Playwright 246/180-pass/15-baseline,
  test_pwa 22.
- **#207 remainder** — the operator delegated it to me ("you can do all of
  these"). It's doable WITHOUT the org runner list: reword the "prohibited
  estate-wide" comments to fork-friendly wording, drop the `node-b` label for
  capability-only labels, vendor/remove the private runner-policy reusable
  workflow, and fix stale doc pointers (release.yml `DEPLOYMENT.md §9.6`→§7;
  build.yml/ci.yml cite the removed `REQUIREMENTS.md`). Queued after the deploy
  + #293.

### Post-Phase-1 direction (operator, 2026-08-23)

Operator answered the phase-boundary question: **build #293** next, and **"you
can do all of these"** (the operator-unblock items — #207, redeploy, #295/#317
secrets) with **"I'll phone-smoke when you deploy."** So:

- **DEPLOY vogt-dev — ✅ LIVE (recovered 2026-08-23 10:02).** dev tip `8533c5c` (all Phase-1 work: new
  PWA identity rename #271, transport retry #198, config #208) is building
  (`build.yml`, self-hosted). Mechanism per [[vogt-dev-deploy-mechanism]]:
  Komodo `UpdateStack` the two pinned env digests (`VOGT_STACK_IMAGE`,
  `VOGT_IMAGE`) + `DeployStack` on Node B. Current (rollback) digests recorded
  in scratchpad `deploy-rollback.md`; the live stack was on `deployed_hash
  6539234` (pre-Phase-1, #157 era). The env holds live secrets → the digest
  swap is done programmatically (GetStack→sed two lines→UpdateStack) so secrets
  never leave the pipe. **Deploy kills any coding session inside vogt-dev** —
  it's the last action; operator does the phone smoke after.

  **⚠ THE DIGEST BUMP ALONE BROKE THE STACK — 501 restarts over 2.5 h.** The
  image swap landed at 07:24 and `vogt-dev` crash-looped until 10:02. Root
  cause is the two-repo compose drift that `vogt-stack.komodo.md` §1 warns
  about, finally biting: the Phase-1 image carries **#205** ("make engine
  agent-auth/MCP/compose estate-neutral"), which moved the Cadastre and
  Infisical agent-auth wiring **out of `agent-auth.sh` and into environment the
  overlay must supply**. `indexarr/ops` `personal/vogt-dev/estate.overlay.yml`
  was still the pre-#205 copy from `6539234` (#157 era), so the new binary hit
  `engine/deploy/agent-auth.sh:198` — `CADASTRE_MCP_ENABLED=1 but
  MYDEVENV2_CADASTRE_SECRET_NAME names no secret` — and
  `MYDEVENV2_AGENT_AUTH_REQUIRED=1` made that fatal, every ~35 s.
  Fixed by syncing the product repo's `deploy/estate.overlay.yml` wholesale to
  the ops copy (ops `6808e84`) — it added `ENGINE_AGENT_AUTH_SECRETS` + its
  project id, `ENGINE_AGENT_AUTH_GH_TOKEN_FROM`, `CADASTRE_MCP_URL`,
  `MYDEVENV2_CADASTRE_SECRET_NAME`, `ENGINE_ALLOWED_ORIGINS`, and the #199
  `VOGT_BOOTSTRAP_CORE_TOKEN_*` block. `vogt.compose.yml` and
  `estate.docker-socket.yml` were already identical; the overlay was the only
  drifted file, and every variable it needs was already in the stack env.
  Verified after redeploy: both containers healthy, `/version` →
  `{"version":"0.2.0","name":"vogt"}`, `/health/ready` → `ready`, and the PWA
  serves `<title>Vogt</title>` (so #271 really is live). **Operator's phone
  smoke is now unblocked.**

  **Rules this earns:**
  1. **Never bump a pinned digest without diffing the ops compose against
     `vogt/deploy/` first.** `diff ops/personal/vogt-<env>/estate.overlay.yml
     vogt/deploy/estate.overlay.yml` is a one-line pre-flight; skipping it cost
     2.5 h of downtime. An image and its compose are one artifact across two
     repos, and only the image half gets bumped automatically.
  2. **`vogt-prod` carries the identical drift and is one image bump from the
     same outage.** It survives only because it still runs a pre-#205 image.
     Sync `personal/vogt-prod/estate.overlay.yml` *before* prod is next
     deployed, not during.
  3. **The `vogt-dev` webhook did not fire on push to ops `main`**, contrary to
     `vogt-stack.komodo.md` §2's table. An explicit `POST /execute/DeployStack`
     was needed. Do not assume the push is the deploy — poll `GetStack`'s
     `deployed_hash` and call `DeployStack` if it hasn't moved.
  4. **`GetStack` returns the whole stack environment inline**, so the Infisical
     client secret, the OpenRouter key and the Firebase service-account private
     key land in the transcript of anyone who calls it. Filter the response to
     the fields you need; never dump `info.deployed_config` or
     `config.environment` whole. Those three are worth rotating.
- **#293 external workflow-engine backend — increment-1 DELEGATED** to a
  worktree agent (branch `feat/workflow-engine-backend`, with an explicit
  `reset --hard origin/dev` stale-base guard so the #271 worktree bug can't
  recur). Concrete provider is **Fabro** (per `docs/local/FABRO_COMPARISON.md`).
  Increment-1 = the provider-pluggable seam: `WorkflowEngineConfig` on the task,
  dispatch in `start_run_inner`, a `WorkflowProvider` trait + a `FabroProvider`
  built against Fabro's *documented* REST contract and tested with a FAKE axum
  server, absence-is-non-fatal. **Deferred to increment-2** (needs a reachable
  Fabro instance — none is deployed on the estate, an infra/provisioning
  decision): live SSE event mirroring, gate bridging (#289), and #283/#284
  checkpoint-branch collection. `Refs #293`, not `Closes`.

### Session close-out (2026-08-23, post-deploy)

**Merged to `dev` (tip `a809244`):**
- **PR #324 — #293 increment-1** (`feat/workflow-engine-backend`): the
  workflow-engine seam. Verified independently (merge-base == dev tip, no stale
  base; +1180 lines; cargo fmt/clippy/test green incl. 7 new workflow tests).
  Open design note for increment-2: Fabro's future human-gate/interrupted state
  → whether it maps to Vogt's `Blocked` outcome or stays folded into `Failed`
  (decide when the real Fabro contract is verified).
- **PR #325 — #207 finish** (`ci/finish-207-runner-labels`): dropped the
  `node-b` location label from the two straggler workflows (`runner-policy.yml`,
  `pod-base.yml`) to match ci.yml's already-live capability-only policy. The
  rest of #207 (stale REQUIREMENTS.md / DEPLOYMENT.md §9.6 pointers, fork
  comments, inlined runner-policy gate) was already done in #315. `runner-policy`
  + Android jobs both green with the new labels, confirming correct rescheduling.

**Voice / #317 investigation (blocks APK voice; awaiting operator decision):**
The engine voice pipeline is already provider-agnostic — standard OpenAI audio
interface (`/audio/transcriptions`, `/audio/speech`) configured by
`ENGINE_ASSISTANT_STT/TTS_BASE_URLS`/`_API_KEY`/`_MODEL`. Probed the operator's
available providers: **OpenRouter's `/audio/*` are a different shape** (TTS needs
its own model, not `tts-1`; STT wants an `input_audio` JSON object, not the
multipart file the engine sends; its audio is really chat-completions-with-
modalities) and **Gemini's OpenAI-compat layer 404s on `/audio/*`**. So neither
is a config-only drop-in. Open-source-correct #317 (operator agreed genericity
is required): (1) neutralize the dead `api.theclawbay.com` default, (2) ship a
no-account self-hostable OpenAI-compatible Whisper+TTS reference pair (what the
voicemode defaults `127.0.0.1:2022`/`:8880` target), (3) optionally later a
generic chat-completions-audio provider path (covers OpenRouter/Gemini).
**Asked the operator: proceed with (1)+(2) self-host for dev?** — awaiting.

**Deploy (done, via recovery):** vogt-dev live/healthy on `8533c5c` (Phase-1
PWA + `vogt-engine`); operator's phone smoke unblocked. Sign-in token for the
PWA is the **engine** token `apps/prod/MYDEVENV2_TOKEN` (the `vogt_*` core/agent
tokens 401 at the front door). Digest-bump crash-looped 2.5 h on two-repo
overlay drift; recovered by syncing ops `estate.overlay.yml` (#205 keys) — full
account + 4 rules in [[vogt-dev-deploy-mechanism]].

**Open operator decisions (surfaced, not blocking other work):**
- Voice provider path (above) — self-host / OpenAI key / build adapter.
- Rotate the secrets exposed to this session's transcript (`MYDEVENV2_TOKEN`
  engine master, Infisical client secret, OpenRouter key, Firebase key #265,
  plus the three dev `vogt_*` tokens).
- Sync `vogt-prod`'s `estate.overlay.yml` preemptively (verified additive +
  safe; does NOT deploy — prevents a repeat 2.5 h outage on prod's next deploy).

**Logged bug (operator deprioritized — mobile web not a focus):** the "go to"
button in Firefox mobile-mode loops when dismissing its prompt; desktop-mode on
mobile is fine. Focus is the APK + working voice, not mobile-web polish.

**Latent-drift discovery + fix — PR #319 `chore/ruff-format-drift` (MERGED, dev
now `4a849b9`).** #318 first went red on `ruff format --check` in files it never
touched. Root cause: dev tip `d47cc06` was a **docs-only merge**, so CI's
"classify changes" gate skipped the Python lint/type/test job — leaving a
`ruff format` drift (ruff 0.16.2 as pinned in `uv.lock`) from the #284/#285/#294/#296
code merges sitting **latent** on dev across 8 files (`edges.py`,
`application/services/git_story.py`, `core/git_story.py`, `test_drift.py`,
`test_fake_agent.py`, `test_git_story.py`, `test_pr_work_item_edge.py`,
`test_pwa.py`). Formatting-only, merged first; #318 rebased on top and re-runs
green. **Lesson for the log: a docs-only merge to `dev` can green CI while
hiding a Python lint break; the next core PR pays for it — run `uv run ruff
format --check .` locally before any core PR.**
