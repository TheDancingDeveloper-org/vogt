import type { CapacitorConfig } from "@capacitor/cli";

// Vogt — Capacitor Android wrap.
//
// The native server chooser supplies the front door at runtime. No deployment
// address is compiled into the shell. UI and APIs come from that same origin.
// `web/` is the bundled fallback when no server is selected.
//
// ── Names ──────────────────────────────────────────────────────────────────
//
// `appName` is the label under the icon, so it says Vogt.
//
// **`appId` is now `com.thedancingdeveloper.vogt`.** A package name is an
// identity, not a label, so moving it has a real device-side cost: a rename first
// renamed the package to `com.sprooty.vogt`, and the id then moved
// again to `com.thedancingdeveloper.vogt` so the app publishes under the owned
// `thedancingdeveloper.com` domain — the reverse-DNS the Play Store record and
// Play App Signing bind to on first upload. Two consequences are operator
// follow-ups, not oversights: (a) each new id is a *new* app, so there is no
// in-place upgrade — a prior build reinstalls — and (b) FCM will not deliver on
// the new id until the operator adds `com.thedancingdeveloper.vogt` (and
// `.dev`) to the Firebase project and supplies a real
// `android/app/google-services.json`; the committed placeholder there is
// sanitized and non-live. `build.gradle` moves `applicationId` and
// `namespace` in the same change, and this file and that one must keep the same
// fallback id — tests/test_mobile_identity.py asserts it.

const config: CapacitorConfig = {
  appId: process.env.VOGT_ANDROID_APP_ID || "com.thedancingdeveloper.vogt",
  appName: process.env.VOGT_ANDROID_APP_NAME || "Vogt",
  webDir: "web",
  // Page zoom is off: with it on, a pinch over a terminal zoomed the
  // whole WebView and the drag that followed panned the zoomed page instead
  // of scrolling the buffer. Text size is a product setting (terminal font
  // size, app theme), not a viewport gesture.
  zoomEnabled: false,
  android: {
    zoomEnabled: false,
  },
  server: {
    // Users may explicitly select an HTTP front door on a private network.
    cleartext: true,
    androidScheme: "https",
  },
  plugins: {
    PushNotifications: {
      presentationOptions: ["badge", "sound", "alert"],
    },
  },
};

export default config;
