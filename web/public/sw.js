// MyDevEnv2 service worker — handles push events, notification clicks, and an
// explicit offline fallback for navigation requests. The app remains online-only;
// we do not cache an app shell for disconnected use.

const OFFLINE_HTML = `<!doctype html>
<html lang="en">
  <head>
    <meta charset="utf-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1" />
    <title>MyDevEnv2 Offline</title>
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
      <p>MyDevEnv2 needs a live server connection. Reconnect to the network and try again.</p>
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
    payload = { title: "MyDevEnv2", body: event.data.text() };
  }
  const title = payload.title || "MyDevEnv2";
  const opts = {
    body: payload.body || "",
    icon: "/favicon.svg",
    badge: "/favicon.svg",
    data: payload.data || {},
    tag: payload.data?.session_id || "mydevenv2-default",
    renotify: true,
  };
  event.waitUntil(self.registration.showNotification(title, opts));
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
