// Roost Web Push service worker. Payload:
// { sessionId, kind: "blocked" | "done", title, body }

self.addEventListener("push", (event) => {
  if (!event.data) return;
  let payload;
  try {
    payload = event.data.json();
  } catch {
    return;
  }
  if (
    !payload
    || typeof payload.sessionId !== "string"
    || (payload.kind !== "blocked" && payload.kind !== "done")
  ) return;

  const sessionId = payload.sessionId;
  const title = typeof payload.title === "string" ? payload.title.slice(0, 160) : "Roost";
  const body = typeof payload.body === "string" ? payload.body.slice(0, 512) : "";
  event.waitUntil(self.registration.showNotification(title || "Roost", {
    body,
    tag: `roost-agent:${sessionId}`,
    data: { sessionId },
    requireInteraction: payload.kind === "blocked",
    icon: "/icon-192.png?v=2",
    badge: "/icon-32.png?v=2",
  }));
});

self.addEventListener("notificationclick", (event) => {
  event.notification.close();
  const value = event.notification.data?.sessionId;
  const sessionId = typeof value === "string" ? value : undefined;
  const target = sessionId ? `/s/${encodeURIComponent(sessionId)}` : "/";
  event.waitUntil((async () => {
    const windows = await self.clients.matchAll({ type: "window", includeUncontrolled: true });
    for (const client of windows) {
      if (!("focus" in client)) continue;
      await client.focus();
      if (sessionId) client.postMessage({ type: "roost-navigate", sessionId });
      return;
    }
    await self.clients.openWindow?.(target);
  })());
});
