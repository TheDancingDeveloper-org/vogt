// Vogt service worker — handles push events, notification clicks, and an
// explicit offline fallback for navigation requests. The app remains online-only;
// we do not cache an app shell for disconnected use.

const OFFLINE_HTML = `<!doctype html>
<html lang="en">
  <head>
    <meta charset="utf-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1" />
    <title>Offline · Vogt</title>
    <style>
      :root {
        color-scheme: dark;
        font-family: Inter, system-ui, sans-serif;
      }
      body {
        margin: 0;
        min-height: 100vh;
        display: grid;
        place-items: center;
        background: #0d1117;
        color: #f0f6fc;
      }
      main {
        width: min(420px, calc(100vw - 32px));
      }
      h1 {
        margin: 0 0 12px;
        font-size: 24px;
      }
      p {
        margin: 0 0 18px;
        color: #8b949e;
        line-height: 1.5;
      }
      button {
        appearance: none;
        border: 1px solid rgba(240, 246, 252, 0.16);
        background: #21262d;
        color: inherit;
        border-radius: 8px;
        padding: 10px 14px;
        font: inherit;
      }
    </style>
  </head>
  <body>
    <main>
      <h1>Connection required</h1>
      <p>Vogt needs a live server connection. Reconnect to the network and try again.</p>
      <button type="button" onclick="location.reload()">Retry</button>
    </main>
  </body>
</html>`;

self.addEventListener("install", (event) => {
  self.skipWaiting();
});

self.addEventListener("activate", (event) => {
  event.waitUntil(
    (async () => {
      const keys = await caches.keys();
      await Promise.all(keys.map((key) => caches.delete(key)));
      await self.clients.claim();
    })(),
  );
});

self.addEventListener("fetch", (event) => {
  if (event.request.mode !== "navigate") return;
  event.respondWith(
    (async () => {
      try {
        return await fetch(event.request);
      } catch {
        return new Response(OFFLINE_HTML, {
          headers: {
            "Content-Type": "text/html; charset=utf-8",
            "Cache-Control": "no-store",
          },
        });
      }
    })(),
  );
});

self.addEventListener("push", (event) => {
  if (!event.data) return;
  let payload;
  try {
    payload = event.data.json();
  } catch {
    payload = { title: "Vogt", body: event.data.text() };
  }
  const title = payload.title || "Vogt";
  const opts = {
    body: payload.body || "",
    icon: "/icon-192.png",
    badge: "/notification-badge.svg",
    data: payload.data || {},
    tag: payload.data?.session_id || "mydevenv2-default",
    renotify: true,
  };
  event.waitUntil(self.registration.showNotification(title, opts));
});

// The browser can rotate or expire a push subscription without the tab ever
// coming to the foreground — a re-keyed VAPID pair, a pushed-out expiry. When
// that happens the old endpoint stops delivering silently. Resubscribe and
// re-register the fresh subscription so this device keeps receiving, without
// waiting for the user to reopen Settings.
function urlBase64ToUint8Array(base64String) {
  const padding = "=".repeat((4 - (base64String.length % 4)) % 4);
  const b64 = (base64String + padding).replace(/-/g, "+").replace(/_/g, "/");
  const raw = atob(b64);
  const out = new Uint8Array(raw.length);
  for (let i = 0; i < raw.length; i++) out[i] = raw.charCodeAt(i);
  return out;
}

self.addEventListener("pushsubscriptionchange", (event) => {
  event.waitUntil(
    (async () => {
      try {
        let sub = event.newSubscription || null;
        if (!sub) {
          let applicationServerKey =
            event.oldSubscription?.options?.applicationServerKey || null;
          if (!applicationServerKey) {
            const res = await fetch("/api/push/public-key");
            if (res.ok) {
              const { vapid_public_key } = await res.json();
              if (vapid_public_key) {
                applicationServerKey = urlBase64ToUint8Array(vapid_public_key);
              }
            }
          }
          sub = await self.registration.pushManager.subscribe({
            userVisibleOnly: true,
            applicationServerKey: applicationServerKey || undefined,
          });
        }
        const json = sub.toJSON();
        await fetch("/api/push/subscribe", {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({
            kind: "web-push",
            endpoint: json.endpoint,
            p256dh: json.keys?.p256dh,
            auth: json.keys?.auth,
          }),
        });
      } catch {
        // Best effort: the next foreground refresh reconciles against the
        // server list and offers Re-enable if this failed.
      }
    })(),
  );
});

self.addEventListener("notificationclick", (event) => {
  event.notification.close();
  const url = event.notification.data?.url || "/";
  event.waitUntil(
    (async () => {
      const all = await self.clients.matchAll({
        type: "window",
        includeUncontrolled: true,
      });
      for (const c of all) {
        if ("focus" in c) {
          c.postMessage({ type: "navigate", url });
          return c.focus();
        }
      }
      if (self.clients.openWindow) {
        return self.clients.openWindow(url);
      }
    })(),
  );
});
