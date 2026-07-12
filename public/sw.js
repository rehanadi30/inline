// Service worker for inline.
//
// - Notifications: shown via the service worker so they also work on mobile
//   browsers that block the page-level Notification constructor.
// - Offline: caches the app shells and the latest GET responses so the apps
//   still load and show the last-known state when the server is unreachable.
//   Mutations (POST) are never cached; they fail while offline and the UI says so.
//
// Bump CACHE after changing the apps to invalidate old caches.

const CACHE = "inline-v2";
const SHELL = ["/", "/index.html", "/admin.html"];

self.addEventListener("install", (event) => {
  self.skipWaiting();
  event.waitUntil(caches.open(CACHE).then((c) => c.addAll(SHELL).catch(() => {})));
});

self.addEventListener("activate", (event) => {
  event.waitUntil((async () => {
    const keys = await caches.keys();
    await Promise.all(keys.filter((k) => k !== CACHE).map((k) => caches.delete(k)));
    await self.clients.claim();
  })());
});

self.addEventListener("fetch", (event) => {
  const req = event.request;
  const url = new URL(req.url);

  // Pass through non-GET, the SSE stream, and the health ping (health must hit
  // the network so the app can detect downtime).
  if (req.method !== "GET" || url.pathname === "/api/events" || url.pathname === "/api/health") {
    return;
  }

  const isApi = url.pathname.startsWith("/api/");
  const isNav = req.mode === "navigate";

  if (isNav || isApi) {
    // Network-first, falling back to cache when offline.
    event.respondWith((async () => {
      try {
        const res = await fetch(req);
        if (res && res.ok) {
          const c = await caches.open(CACHE);
          c.put(req, res.clone());
        }
        return res;
      } catch (err) {
        const cached = await caches.match(req);
        if (cached) return cached;
        if (isNav) {
          const shell = (await caches.match("/admin.html")) || (await caches.match("/"));
          if (shell) return shell;
        }
        throw err;
      }
    })());
    return;
  }

  // Other static assets: cache-first.
  event.respondWith(caches.match(req).then((c) => c || fetch(req)));
});

self.addEventListener("notificationclick", (event) => {
  event.notification.close();
  const target = (event.notification.data && event.notification.data.url) || "/";
  event.waitUntil((async () => {
    const wins = await clients.matchAll({ type: "window", includeUncontrolled: true });
    for (const w of wins) {
      if ("focus" in w) { try { await w.focus(); } catch (_) {} return; }
    }
    if (clients.openWindow) return clients.openWindow(target);
  })());
});

// Web Push handler (optional). Sending pushes requires VAPID keys and a server
// endpoint; see CUSTOMIZE.md.
self.addEventListener("push", (event) => {
  let data = {};
  try { data = event.data ? event.data.json() : {}; } catch (_) {}
  event.waitUntil(
    self.registration.showNotification(data.title || "inline", {
      body: data.body || "",
      tag: data.tag || "inline-push",
      renotify: true,
      data: { url: data.url || "/" },
    })
  );
});
