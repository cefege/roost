// Roost Web Push service worker, registered by crate::web_push at scope "/".
// Payloads:
// - session (roost-coord push/dispatch.rs AgentPushPayload and the session pushes):
//   { sessionId, kind: "blocked" | "done", title, body }
// - pairing (roost-coord push/pair_request.rs PairRequestPayload):
//   { kind: "pair_request", ephemeralId, title, body }

// An updated worker replaces the one a browser already runs at once, rather
// than after every Roost window has closed.
self.addEventListener("install", () => {
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
    return;
  }
  if (!payload) return;
  const title = typeof payload.title === "string" ? payload.title.slice(0, 160) : "Roost";
  const body = typeof payload.body === "string" ? payload.body.slice(0, 512) : "";

  if (payload.kind === "pair_request" && typeof payload.ephemeralId === "string") {
    event.waitUntil(self.registration.showNotification(title || "Roost", {
      body,
      tag: `roost-pair:${payload.ephemeralId}`,
      data: { target: "pair" },
      requireInteraction: true,
      icon: "/icon-192.png?v=2",
      badge: "/icon-32.png?v=2",
    }));
    return;
  }

  if (
    typeof payload.sessionId !== "string"
    || (payload.kind !== "blocked" && payload.kind !== "done")
  ) return;

  const sessionId = payload.sessionId;
  event.waitUntil(self.registration.showNotification(title || "Roost", {
    body,
    tag: `roost-agent:${sessionId}`,
    data: { sessionId },
    requireInteraction: payload.kind === "blocked",
    icon: "/icon-192.png?v=2",
    badge: "/icon-32.png?v=2",
  }));
});

// A click focuses an open Roost window and asks it to route in place (the app
// listens for "roost-navigate" and keeps its live state); with no window open
// it opens the session's URL, or /pair for a pairing request.
self.addEventListener("notificationclick", (event) => {
  event.notification.close();
  const pairing = event.notification.data?.target === "pair";
  const value = event.notification.data?.sessionId;
  const sessionId = typeof value === "string" ? value : undefined;
  const target = pairing ? "/pair" : sessionId ? `/s/${encodeURIComponent(sessionId)}` : "/";
  event.waitUntil((async () => {
    const windows = await self.clients.matchAll({ type: "window", includeUncontrolled: true });
    const client = windows.find((candidate) => candidate.focused)
      ?? windows.find((candidate) => candidate.visibilityState === "visible")
      ?? windows[0];
    if (client) {
      if (pairing) client.postMessage({ type: "roost-navigate", target: "pair" });
      else if (sessionId) client.postMessage({ type: "roost-navigate", sessionId });
      if ("focus" in client) await client.focus();
      return;
    }
    await self.clients.openWindow?.(target);
  })());
});
