import type { CapacitorConfig } from "@capacitor/cli";

// MyDevEnv2 — Capacitor Android wrap.
//
// The WebView loads the deployed server URL directly, so UI updates ship
// without rebuilding the APK. The local `web/` dir holds a tiny fallback
// page used if the server is unreachable on first load (Capacitor requires
// a webDir even when server.url is set).
//
// Switch `server.url` to a Tailscale-only URL like `http://mydevenv2:8910`
// if you want the app to never traverse the public internet — works as
// long as the phone is on the tailnet (Tailscale's Android app must be
// running). For convenience the default points at the Caddy-fronted name
// which works both publicly (with basic_auth) and on the tailnet.
//
// appId/appName/server.url are env-var-overridable so the same source tree
// builds either the prod or dev flavor (see .woodpecker/server.yml
// mobile-apk / mobile-apk-dev) — a distinct appId lets both APKs install
// side by side on one device. build.gradle's `applicationId` override must
// be kept in sync with MYDEVENV2_ANDROID_APP_ID (Capacitor's appId does not
// itself rewrite an already-scaffolded build.gradle).

const config: CapacitorConfig = {
  appId: process.env.MYDEVENV2_ANDROID_APP_ID || "com.sprooty.mydevenv2",
  appName: process.env.MYDEVENV2_ANDROID_APP_NAME || "MyDevEnv2",
  webDir: "web",
  zoomEnabled: true,
  android: {
    zoomEnabled: true,
  },
  server: {
    url: process.env.MYDEVENV2_ANDROID_SERVER_URL || "https://mydevenv2.sprooty.com",
    cleartext: false,
    androidScheme: "https",
  },
  plugins: {
    PushNotifications: {
      presentationOptions: ["badge", "sound", "alert"],
    },
  },
};

export default config;
