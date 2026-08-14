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
// It used to default to `https://mydevenv2.sprooty.com`, and that default is
// what FR-M1 is against. That host is MyDevEnv2's own standalone stack: it
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
// M14 settled §11.1: the merged product is **Vogt**, served at
// `vogt.sprooty.com`. `appName` is the label under the icon, so it says Vogt
// — a phone showing "MyDevEnv2" beside a product that calls itself something
// else is the naming decision failing at the only place a user reads it.
//
// **`appId` stays `com.sprooty.mydevenv2`, and that is not an oversight.** A
// package name is an identity, not a label: `android/app/google-services.json`
// is keyed to it, so changing it invalidates FCM registration for every
// installed device; `build.gradle`'s `applicationId` would have to move in the
// same commit; and the store/sideload result is a *second* app beside the
// first rather than an upgrade of it. Renaming it is a migration with a
// device-side cost, and it belongs with the FCM project decision rather than
// with a URL change.

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
  appId: process.env.MYDEVENV2_ANDROID_APP_ID || "com.sprooty.mydevenv2",
  appName:
    process.env.VOGT_ANDROID_APP_NAME ||
    // The transition alias, as with the server URL above.
    process.env.MYDEVENV2_ANDROID_APP_NAME ||
    "Vogt",
  webDir: "web",
  zoomEnabled: true,
  android: {
    zoomEnabled: true,
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
