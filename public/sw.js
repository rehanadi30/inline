// inline — service worker
//
// Why this exists: on mobile browsers (e.g. Android Chrome) the page-level
// `new Notification()` API is blocked; notifications must be shown through a
// Service Worker registration. The customer app registers this file and calls
// `registration.showNotification(...)`. This worker also focuses/opens the
// ticket page when a notification is tapped.
//
// It intentionally does NOT cache anything (the app must always show live
// data). For alerts when the app is fully closed, add Web Push below.

self.addEventListener("install", () => self.skipWaiting());
self.addEventListener("activate", (event) => event.waitUntil(self.clients.claim()));

// Tapping a notification focuses an existing tab, or opens the ticket page.
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

// ── OPTIONAL: Web Push (alerts even when the app is fully closed) ──────────
// Requires a push service + VAPID keys + a backend endpoint to send pushes.
// See CUSTOMIZE.md. The handler is ready; wiring the server side is up to you.
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
