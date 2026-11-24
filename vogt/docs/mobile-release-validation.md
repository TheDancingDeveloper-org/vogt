# Mobile release validation

Instrumentation and manual checks that back the mobile shell's Android
lifecycle, connectivity, and voice coverage. The automated tiers run in
[`.github/workflows/android-instrumentation.yml`](../.github/workflows/android-instrumentation.yml);
the tiers that cannot close from CI are the operator checklists below.

## Automated (emulator) tier

`android-instrumentation.yml` runs the whole `androidTest` suite via
`:app:connectedDebugAndroidTest` on an API 34 x86_64 emulator. It is
`workflow_dispatch`-only until a KVM-capable self-hosted runner is registered to
the repo; once one is, path-filter it to `mobile/**` on push/pull_request to make
it a per-change gate.

Covered there:

- **`VoiceServiceLifecycleTest`** — foreground-service start/stop, the
  notification "End conversation" action signalling the PWA, a mic-less start
  degrading without a crash, and background/foreground survival.
- **`VoiceServicePermissionDenialTest`** — a notification
  (FCM / `POST_NOTIFICATIONS`) denial and a combined mic-and-notification denial
  degrade without a crash.
- **`VoiceServiceProcessReclaimTest`** — the held service re-registers cleanly
  after a simulated reclaim and leaves no stale or duplicate service.
- **`VoiceServiceScreenOffTest`** — the started foreground service outlives the
  Activity being backgrounded (the deterministic screen-off proxy).

Shared helpers live in `VoiceServiceTestSupport`.

## Manual / device tier

These need a real device (Doze, radios, battery) or the Play Console and are not
reproducible from instrumentation. Record the outcome in the release checklist
for the version being shipped.

### Screen-off survival (device)

- [ ] Start a voice conversation, turn the screen off, and confirm after 30
      minutes that the assistant socket is still connected and TTS/audio still
      work. The service logs conversation start/end to logcat
      (`VoiceConversation`) to bound the window.
- [ ] Confirm an FCM alert delivered while the screen is off still surfaces
      (with `POST_NOTIFICATIONS` granted).

### Connectivity / VPN loss (device)

- [ ] Drop and restore Wi-Fi mid-conversation; confirm the WebView reconnects
      and the native service does not go stale.
- [ ] Toggle a VPN on/off mid-conversation; confirm the same.

### Process-reclaim (device)

- [ ] Force-stop or let the OS reclaim the app mid-conversation, reopen it, and
      confirm the PWA reconnects and no orphaned foreground notification remains.
      (The deterministic native half — clean re-registration, no stale service —
      is covered by `VoiceServiceProcessReclaimTest`.)

### Terminal resume and swipe smoke (device, WI-2709)

The mocked counterpart is the `@touch` set in `web/tests/browser/gui.spec.ts`,
run on the `phone-android` (Pixel 7) and `phone` Playwright projects in the
merge queue. This checks the same contract on a real Android browser or
WebView, where backgrounding really freezes the page and the network may drop.

Where: the Windows box emulator (desktop-tlk1gff, see the
`remote-android-emulator` skill) or the operator's phone over USB debugging.
Needs `adb` and a desktop Chrome for `chrome://inspect`.

1. Open the PWA (or the dev shell app) signed in, with at least two live
   sessions, on `#/t/<session>`.
2. Attach DevTools: `adb forward tcp:9222 localabstract:chrome_devtools_remote`
   (for the app's WebView: `localabstract:webview_devtools_remote_<pid>`), then
   open `chrome://inspect` and inspect the page. In **Network**, filter `attach`
   and select **WS**, then clear the list.
3. **Resume:** `adb shell input keyevent KEYCODE_HOME`, wait 60 s,
   then bring the browser back (`adb shell monkey -p com.android.chrome 1`, or
   the app package). Pass when:
   - [ ] at most **one** new `…/attach` socket appears for the session
         (none if the old one survived), and
   - [ ] the terminal never shows the `Reconnecting (try N, …)` overlay or the
         event-stream "Trying to reconnect" banner. Watch the screen, or run
         `adb shell screenrecord /sdcard/resume.mp4` across the resume.
4. **Swipe:** clear the Network list, then swipe the session bar left
   (`adb shell input swipe 900 <bar-y> 150 <bar-y> 250`, with `<bar-y>` the
   session header's y in device pixels), then right. Pass when:
   - [ ] each swipe lands on the neighbouring session, and
   - [ ] each swipe adds **exactly one** `…/attach` socket, for the
         destination session.
5. Record the device, browser or app version, and the socket counts in the
   release checklist. If a check fails once and passes on retry, record it as
   flaky and raise a Vogt item. Don't drop the check.

Until WI-2706 (pager hand-off) lands, step 4 is expected to show two attaches
per swipe. The mocked spec marks this as an expected failure (`test.fail`).

### Dev app identity (side-by-side with prod)

The dev shell is a separate Play record and a separate app on the device, so
it must be telling apart on the home screen, not only by package name. Three
build inputs carry its identity; `release-mobile-dev.yml` sets all three and
`release.yml` sets none (prod takes the defaults):

| Variable | Read by | Default | Dev workflow |
|---|---|---|---|
| `VOGT_ANDROID_APP_ID` | `build.gradle`, `capacitor.config.ts` | `com.thedancingdeveloper.vogt` | `com.thedancingdeveloper.vogt.dev` |
| `VOGT_ANDROID_APP_NAME` | `build.gradle` (generates `app_name`), `capacitor.config.ts` | `Vogt` | `Vogt Dev` |
| `VOGT_ANDROID_APP_ICON` | `build.gradle` (manifest `icon`/`roundIcon`) | `default` | `dev` (amber launcher set, `res/mipmap-*/ic_launcher_dev*`) |

The dev AAB's `versionCode` is `<product semver code> × 10000 + <run number>`
and its `versionName` is `<product version>-dev.<run number>`, computed in the
workflow, so a dev re-upload between product releases is never refused for a
version code Play has already seen. `tests/test_mobile_identity.py` holds
the three inputs and the workflow to this.

### Play preflight (automated, before every upload)

`release-mobile.yml` and `release-mobile-dev.yml` run
[`scripts/play_preflight.py`](../scripts/play_preflight.py) on the built AAB
before `fastlane supply`. A failed check fails the job before anything is
uploaded, prints an `::error::` per failure, and writes a table of every
check to the run's job summary; a further `report the failure` step states in
the summary that no bundle reached Play. The checks:

| Check | Source | Fails when |
|---|---|---|
| package | `bundletool dump manifest` | the bundle's package is not the one the workflow uploads to |
| minSdk | manifest `uses-sdk` | below 24 (Play automatic protection; v0.7.1/v0.7.2 were rejected for 23) |
| targetSdk | manifest `uses-sdk` | below `PLAY_TARGET_SDK_FLOOR` (36 since 2026-08-31) |
| launcher icon | manifest + bundle entries | no MAIN/LAUNCHER activity, no `android:icon`, or the icon resource is absent |
| signing certificate | `keytool -printcert -jarfile` | the signer's SHA-256 differs from the configured upload-key fingerprint |
| Play service-account access | androidpublisher `edits.insert` | the account cannot open an edit on the app (403 → grant it in Play Console → Users and permissions; 404 → the app record does not exist) |
| versionCode | `edits.tracks.list` + `edits.bundles.list` | not greater than every code on any track or ever uploaded |

The Play API half runs only when upload is armed (`VOGT_PLAY_PUBLISH` /
`VOGT_PLAY_PUBLISH_DEV`), because it needs the
`VOGT_PLAY_SERVICE_ACCOUNT_JSON` service account; dry runs report it as
skipped. The edit it opens is always deleted, never committed. The
signing comparison reads the repository variables
`VOGT_PLAY_UPLOAD_CERT_SHA256` (prod) and `VOGT_PLAY_DEV_UPLOAD_CERT_SHA256`
(dev; ignored while the dev build uses a throwaway key). Unset, the check
is reported as a warning and does not block. Set each to the upload
certificate's SHA-256 as Play Console shows it (Setup → App integrity →
Upload key certificate), or from
`keytool -printcert -jarfile <a signed AAB>`; colons are optional.

The script downloads `bundletool` at a pinned version and sha256 into
`$RUNNER_TEMP`, and needs `java`, `keytool` and `openssl` on the runner, all
of which a signing job already has. Two values age and are bumped by hand:
`PLAY_TARGET_SDK_FLOOR` every August when Play raises the target-API
requirement, and `BUNDLETOOL_VERSION`/`BUNDLETOOL_SHA256` together. To check
a bundle locally:

```sh
python3 scripts/play_preflight.py --aab app-release.aab \
  --package com.thedancingdeveloper.vogt \
  --expected-cert-sha256 <fingerprint> \
  [--service-account play-sa.json]
```

### Play internal-track / pre-launch report

The Play Console pre-launch report and internal-track validation are an external
gate — they need the Play Console and a service-account upload, so they cannot
run from the emulator job. Per release:

- [ ] Upload the signed release AAB/APK to the Play **internal** track.
- [ ] Wait for the **pre-launch report** to complete and review it for:
  - [ ] Stability — no crashes/ANRs on the device robo run.
  - [ ] Permissions — `RECORD_AUDIO`, `POST_NOTIFICATIONS`, and the
        `FOREGROUND_SERVICE_*` set are declared and justified.
  - [ ] Accessibility and security warnings triaged.
- [ ] Record the pre-launch report link and verdict in the release notes /
      validation record for the shipped version.

Automating the upload+poll (a Play Developer API service account, e.g. via a
`workflow_dispatch` release-validation job) is a follow-up; until then this stays
an operator step.
