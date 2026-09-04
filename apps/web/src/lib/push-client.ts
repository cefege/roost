// Web Push subscription lifecycle. The desktop-notification preference gates
// the subscription row itself; the coordinator suppresses devices actively
// viewing the affected terminal.

import { coordClient } from "../connect.ts";

const SERVICE_WORKER_URL = "/sw-push.js";

export function pushAvailable(): boolean {
  return (
    typeof navigator !== "undefined"
    && "serviceWorker" in navigator
    && typeof window !== "undefined"
    && "PushManager" in window
    && "Notification" in window
  );
}

function urlBase64ToUint8Array(base64url: string): Uint8Array<ArrayBuffer> {
  const padding = "=".repeat((4 - (base64url.length % 4)) % 4);
  const base64 = (base64url + padding).replace(/-/g, "+").replace(/_/g, "/");
  const raw = atob(base64);
  const output = new Uint8Array(new ArrayBuffer(raw.length));
  for (let index = 0; index < raw.length; index++) output[index] = raw.charCodeAt(index);
  return output;
}

export async function registerPushServiceWorker(): Promise<void> {
  if (!("serviceWorker" in navigator)) return;
  try {
    await navigator.serviceWorker.register(SERVICE_WORKER_URL);
  } catch {
    // Registration is repaired on the next enabled-page load.
  }
}

async function upsertSubscription(subscription: PushSubscription): Promise<void> {
  const keys = subscription.toJSON().keys ?? {};
  if (!keys.p256dh || !keys.auth) {
    throw new Error("Push subscription is missing encryption keys.");
  }
  await coordClient.pushSubscribe({
    endpoint: subscription.endpoint,
    p256dh: keys.p256dh,
    auth: keys.auth,
  });
}

export async function subscribeToPush(): Promise<void> {
  if (!pushAvailable()) {
    throw new Error("Push notifications aren't available in this browser.");
  }
  const permission = await Notification.requestPermission();
  if (permission !== "granted") {
    throw new Error("Allow notifications for Roost in the browser's site settings.");
  }

  const config = await coordClient.pushGetConfig({});
  if (!config.available || !config.vapidPublicKeyB64) {
    throw new Error("Push notifications aren't available on this coordinator.");
  }

  await registerPushServiceWorker();
  const registration = await navigator.serviceWorker.ready;
  const existing = await registration.pushManager.getSubscription();
  const subscription = existing ?? await registration.pushManager.subscribe({
    userVisibleOnly: true,
    applicationServerKey: urlBase64ToUint8Array(config.vapidPublicKeyB64),
  });
  await upsertSubscription(subscription);
}

export interface PushUnsubscribeClient {
  pushUnsubscribe(request: { endpoint: string }): Promise<unknown>;
}

export async function unsubscribeFromPush(
  options: {
    waitForRegistration?: boolean;
    client?: PushUnsubscribeClient;
  } = {},
): Promise<void> {
  if (!("serviceWorker" in navigator)) return;
  const existing = await navigator.serviceWorker.getRegistration(SERVICE_WORKER_URL);
  const registration = existing
    ?? (options.waitForRegistration === false ? undefined : await navigator.serviceWorker.ready);
  const subscription = await registration?.pushManager.getSubscription();
  if (!subscription) return;
  const endpoint = subscription.endpoint;
  try { await subscription.unsubscribe(); } catch { /* best effort */ }
  try { await (options.client ?? coordClient).pushUnsubscribe({ endpoint }); } catch { /* best effort */ }
}

/** Repair an enabled subscription after browser storage or server pruning. */
export async function ensurePushSubscription(): Promise<void> {
  if (!pushAvailable() || Notification.permission !== "granted") return;
  await registerPushServiceWorker();
  const registration = await navigator.serviceWorker.ready;
  const subscription = await registration.pushManager.getSubscription();
  if (subscription) {
    try { await upsertSubscription(subscription); } catch { /* repair next load */ }
    return;
  }
  try { await subscribeToPush(); } catch { /* Settings surfaces explicit errors */ }
}
