// Web Push service worker for Roost OS notifications. Registered by
// src/lib/push-client.ts and served static by coord's SPA asset handler (no
// build step — plain vanilla JS). Two responsibilities:
//   - `push`: render the OS notification from the coord payload.
//   - `notificationclick`: focus an open Roost tab (and tell it to navigate) or
//     open a new one straight at the session.
//
// Payload shape (see apps/coord/src/push-dispatch.ts):
//   { sessionId, kind: "blocked" | "done", title, body }

self.addEventListener("push", (event) => {
  if (!event.data) return;
  let payload;
  try {
    payload = event.data.json();
  } catch {
    return;
  }
  const { sessionId, kind, title, body } = payload;
  event.waitUntil(
    self.registration.showNotification(title || "Roost", {
      body: body || "",
      tag: sessionId, // coalesce: a newer notification for the same session replaces the prior one
      data: { sessionId },
      requireInteraction: kind === "blocked", // blocked persists until acted on; done auto-dismisses
      icon: "/icon-192.png?v=2",
      badge: "/icon-32.png?v=2",
    }),
  );
});

self.addEventListener("notificationclick", (event) => {
  event.notification.close();
  const sessionId = event.notification.data && event.notification.data.sessionId;
  const target = sessionId ? "/s/" + sessionId : "/";
  event.waitUntil(
    self.clients
      .matchAll({ type: "window", includeUncontrolled: true })
      .then((windows) => {
        for (const client of windows) {
          if ("focus" in client) {
            client.focus();
            if (sessionId) client.postMessage({ type: "roost-navigate", sessionId });
            return undefined;
          }
        }
        return self.clients.openWindow ? self.clients.openWindow(target) : undefined;
      }),
  );
});
