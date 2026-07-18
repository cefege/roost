// Web Push sender. Encrypts + delivers payloads to stored subscriptions via the
// web-push package (RFC 8291 payload encryption + RFC 8292 VAPID headers).
// VAPID details are configured lazily from the persisted keypair (vapid.ts) on
// first send. Delivery is best-effort: a dead subscription (HTTP 404/410) is
// pruned from push_subscriptions; any other failure is logged, never thrown, so
// one bad endpoint can't block delivery to the others.

import type { Kysely } from "kysely";
import type { DB, PushSubscriptionsTable } from "./db/schema.ts";
import webpush, { WebPushError } from "web-push";
import { getVapidKeys } from "./vapid.ts";
import { log } from "@roost/shared/log";

const TTL_SECONDS = 60;

let _configured = false;

async function ensureConfigured(db: Kysely<DB>): Promise<void> {
  if (_configured) return;
  const keys = await getVapidKeys(db);
  webpush.setVapidDetails("mailto:roost@local", keys.publicKey, keys.privateKey);
  _configured = true;
}

/**
 * Encrypt + POST `payload` to each subscription. Prunes rows whose endpoint the
 * push service reports as gone (404/410). Never throws.
 */
export async function sendPushToSubscriptions(
  db: Kysely<DB>,
  subs: PushSubscriptionsTable[],
  payload: object,
): Promise<void> {
  if (subs.length === 0) return;
  await ensureConfigured(db);
  const body = JSON.stringify(payload);

  await Promise.all(
    subs.map(async (sub) => {
      try {
        await webpush.sendNotification(
          { endpoint: sub.endpoint, keys: { p256dh: sub.p256dh, auth: sub.auth } },
          body,
          { TTL: TTL_SECONDS },
        );
      } catch (err) {
        const status = err instanceof WebPushError ? err.statusCode : null;
        if (status === 404 || status === 410) {
          await db
            .deleteFrom("push_subscriptions")
            .where("viewer_fp", "=", sub.viewer_fp)
            .where("endpoint", "=", sub.endpoint)
            .execute();
          log.info("push", "subscription_expired", { endpoint: sub.endpoint, status });
        } else {
          log.warn("push", "send_failed", {
            endpoint: sub.endpoint,
            status,
            error: String(err),
          });
        }
      }
    }),
  );
}
