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

const config: CapacitorConfig = {
  appId: "com.sprooty.mydevenv2",
  appName: "MyDevEnv2",
  webDir: "web",
  bundledWebRuntime: false,
  zoomEnabled: true,
  android: {
    zoomEnabled: true,
  },
  server: {
    url: "https://mydevenv2.sprooty.com",
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
