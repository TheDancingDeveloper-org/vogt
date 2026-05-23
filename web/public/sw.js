// MyDevEnv2 service worker — minimal: handles push events + click.
// No offline caching (intentional; the app needs the server to be useful).

self.addEventListener("install", (event) => {
  self.skipWaiting();
});

self.addEventListener("activate", (event) => {
  event.waitUntil(self.clients.claim());
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
