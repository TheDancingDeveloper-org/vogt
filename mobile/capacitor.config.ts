import type { CapacitorConfig } from "@capacitor/cli";

// Vogt — Capacitor Android wrap (FR-M1).
//
// The WebView loads a deployed Vogt front door directly, so UI updates ship
// without rebuilding the APK; `mobile/web/` holds a one-file fallback page
// used when the server is unreachable on first load (Capacitor requires a
// webDir even when server.url is set). The APK needs a rebuild only for
// native plumbing — Capacitor plugins, manifest changes, FCM config.
//
// ── Why the URL has no default ─────────────────────────────────────────────
//
// It used to default to the legacy MyDevEnv2 host, and that default is
// what FR-M1 is against. That host was MyDevEnv2's own standalone stack: it
// serves the session engine without vogt-core, so `/api/vogt` answers 503,
// `vogt.configured` is false, and the four Vogt surfaces the drawer offers —
// board, backlog, projects, audit — render their outage state. An APK built
// on the default was a shell around half the product, and it looked like a
// working app while being one.
//
// The obvious repair is a different hostname, and it is the wrong one. The
// merged stack has no settled public name: `MERGE_MYDEVENV2.md` §11.1 leaves
// the domain question open until M14, `deploy/vogt-stack.compose.yml`
// publishes plain HTTP on a Tailscale address with no Caddy block, and
// `VOGT_PUBLIC_URL` in that stack is deliberately un-defaulted for exactly
// this reason — NFR-D2 (revised r4): allocation values carry defaults,
// exposure values do not. A hostname baked in here is an exposure value, and
// baking in a second one would repeat the mistake with a fresher name on it.
//
// So the build asks. An unset variable is a failed build with a sentence
// explaining what to set, which is the honest outcome: nothing in this tree
// knows where your Vogt lives, and an APK that guesses wrong is one that
// silently talks to the wrong estate.
//
// ── Names: one changed, one deliberately not ───────────────────────────────
//
// M14 settled §11.1: the merged product is **Vogt**. `appName` is the label
// under the icon, so it says Vogt
// — a phone showing "MyDevEnv2" beside a product that calls itself something
// else is the naming decision failing at the only place a user reads it.
//
// **`appId` is now `com.thedancingdeveloper.vogt`.** A package name is an
// identity, not a label, so moving it has a real device-side cost: #271 first
// renamed `com.sprooty.mydevenv2` -> `com.sprooty.vogt`, and the id then moved
// again to `com.thedancingdeveloper.vogt` so the app publishes under the owned
// `thedancingdeveloper.com` domain — the reverse-DNS the Play Store record and
// Play App Signing bind to on first upload. Two consequences are operator
// follow-ups, not oversights: (a) each new id is a *new* app, so there is no
// in-place upgrade — a prior build reinstalls — and (b) FCM will not deliver on
// the new id until the operator adds `com.thedancingdeveloper.vogt` (and
// `.dev`) to the Firebase project and supplies a real
// `android/app/google-services.json`; the committed placeholder there is
// sanitized and non-live (#265). `build.gradle` moves `applicationId` and
// `namespace` in the same change, and this file and that one must keep the same
// fallback id — tests/test_mobile_identity.py asserts it.

const SERVER_URL =
  process.env.VOGT_ANDROID_SERVER_URL ||
  // The transition-period alias §11.1 recommends. Both names are read for as
  // long as the `MYDEVENV2_*` surface is supported; neither is defaulted.
  process.env.MYDEVENV2_ANDROID_SERVER_URL;

if (!SERVER_URL) {
  throw new Error(
    "VOGT_ANDROID_SERVER_URL is not set, so there is no way to know which " +
      "Vogt this APK should load. Set it to the URL of a front door running " +
      "the merged stack — the value that deployment gave VOGT_PUBLIC_URL, " +
      "e.g. http://100.92.54.45:18094 on the tailnet. It is deliberately not " +
      "defaulted: see the comment in mobile/capacitor.config.ts.",
  );
}

const config: CapacitorConfig = {
  appId:
    process.env.VOGT_ANDROID_APP_ID ||
    // The transition alias, as with the server URL and app name above.
    process.env.MYDEVENV2_ANDROID_APP_ID ||
    "com.thedancingdeveloper.vogt",
  appName:
    process.env.VOGT_ANDROID_APP_NAME ||
    // The transition alias, as with the server URL above.
    process.env.MYDEVENV2_ANDROID_APP_NAME ||
    "Vogt",
  webDir: "web",
  // Page zoom is off (#592): with it on, a pinch over a terminal zoomed the
  // whole WebView and the drag that followed panned the zoomed page instead
  // of scrolling the buffer. Text size is a product setting (terminal font
  // size, app theme), not a viewport gesture.
  zoomEnabled: false,
  android: {
    zoomEnabled: false,
  },
  server: {
    url: SERVER_URL,
    // Derived, not decided. The merged stack has no TLS of its own — the
    // engine is the front door and it speaks plain HTTP over WireGuard — so a
    // tailnet deployment is reached at `http://`, and Android blocks
    // cleartext unless it is allowed. Hardcoding `false` here made an
    // `http://` URL fail at runtime with a network error rather than at build
    // time with a reason; deriving it means the flag always matches the URL
    // it was set for.
    //
    // SECURITY (#524.13): an APK built against an `http://` URL submits the
    // bearer token in cleartext over ANY network the device is on, not only
    // over WireGuard. `http://` is acceptable ONLY for a tailnet front door
    // reachable exclusively over the tailnet (the traffic is encrypted by
    // WireGuard, not TLS). Never build an `http://` APK pointed at a host
    // reachable off-tailnet — use `https://` there.
    cleartext: SERVER_URL.startsWith("http://"),
    androidScheme: "https",
  },
  plugins: {
    PushNotifications: {
      presentationOptions: ["badge", "sound", "alert"],
    },
  },
};

export default config;
