// Web Push client — service worker registration + subscription lifecycle.
// Pairs with apps/web/public/sw-push.js (the SW) and the coord Push* RPCs
// (connect.ts::coordClient). This is the SOLE OS-notification path: the old
// in-page new Notification() branch (notifyStore.ts) is gone.
//
// The notifyPrefs().desktop pref gates the SUBSCRIPTION itself, not a per-push
// check: on → a push_subscriptions row exists on coord; off → no row → no push.
// Coord pushes to every stored subscription (minus the viewing-suppression
// rule), so an unsubscribed device simply never gets one.

import { coordClient } from "../connect.ts";

const SW_URL = "/sw-push.js";

/** Whether Web Push is usable in this browser context. */
export function pushAvailable(): boolean {
  return (
    typeof navigator !== "undefined" &&
    "serviceWorker" in navigator &&
    typeof window !== "undefined" &&
    "PushManager" in window &&
    "Notification" in window
  );
}

// applicationServerKey wants a BufferSource (Safari rejects the base64url
// string form), so decode the VAPID public key to raw bytes here.
function urlBase64ToUint8Array(base64url: string): Uint8Array<ArrayBuffer> {
  const padding = "=".repeat((4 - (base64url.length % 4)) % 4);
  const b64 = (base64url + padding).replace(/-/g, "+").replace(/_/g, "/");
  const raw = atob(b64);
  const out = new Uint8Array(new ArrayBuffer(raw.length));
  for (let i = 0; i < raw.length; i++) out[i] = raw.charCodeAt(i);
  return out;
}

/** Register the push SW (idempotent). Safe on load; no permission gate — the SW
 *  must be active before a subscription can be created. */
export async function registerPushServiceWorker(): Promise<void> {
  if (!("serviceWorker" in navigator)) return;
  try {
    await navigator.serviceWorker.register(SW_URL);
  } catch {
    /* SW registration is best-effort */
  }
}

async function upsertSubscription(sub: PushSubscription): Promise<void> {
  const keys = sub.toJSON().keys ?? {};
  if (!keys.p256dh || !keys.auth) throw new Error("Subscription is missing encryption keys.");
  await coordClient.pushSubscribe({ endpoint: sub.endpoint, p256dh: keys.p256dh, auth: keys.auth });
}

/** Full subscribe flow: permission → SW ready → pushManager.subscribe → coord.
 *  Throws a descriptive Error on any failure so callers can surface it. */
export async function subscribeToPush(): Promise<void> {
  if (!pushAvailable()) throw new Error("Push notifications aren't available in this browser.");

  const permission = await Notification.requestPermission();
  if (permission !== "granted") {
    throw new Error(
      "Permission denied — allow notifications for Roost in your browser's site settings.",
    );
  }

  const cfg = await coordClient.pushGetConfig({});
  if (!cfg.available || !cfg.vapidPublicKeyB64) {
    throw new Error("Push is unavailable on the server.");
  }

  await registerPushServiceWorker();
  const reg = await navigator.serviceWorker.ready;
  const existing = await reg.pushManager.getSubscription();
  const sub =
    existing ??
    (await reg.pushManager.subscribe({
      userVisibleOnly: true,
      applicationServerKey: urlBase64ToUint8Array(cfg.vapidPublicKeyB64),
    }));

  await upsertSubscription(sub);
}

/** Unsubscribe everywhere: the browser subscription and the coord row. */
export async function unsubscribeFromPush(): Promise<void> {
  if (!("serviceWorker" in navigator)) return;
  const reg =
    (await navigator.serviceWorker.getRegistration(SW_URL)) ??
    (await navigator.serviceWorker.ready);
  const sub = await reg?.pushManager.getSubscription();
  if (!sub) return;
  const endpoint = sub.endpoint;
  try {
    await sub.unsubscribe();
  } catch {
    /* best effort */
  }
  try {
    await coordClient.pushUnsubscribe({ endpoint });
  } catch {
    /* best effort */
  }
}

/** On load: if the desktop pref is on but no live subscription exists (SW
 *  evicted, storage cleared), re-subscribe silently. If one exists, refresh the
 *  coord row so a server-side prune self-heals. Never throws. */
export async function ensurePushSubscription(): Promise<void> {
  if (!pushAvailable() || Notification.permission !== "granted") return;
  await registerPushServiceWorker();
  const reg = await navigator.serviceWorker.ready;
  const sub = await reg.pushManager.getSubscription();
  if (sub) {
    try {
      await upsertSubscription(sub);
    } catch {
      /* best effort */
    }
    return;
  }
  try {
    await subscribeToPush();
  } catch {
    /* silent on load — the Settings toggle surfaces errors */
  }
}
