// RFC 8291/8292 Web Push delivery. Dead subscriptions are pruned; every
// other delivery failure is isolated so one endpoint cannot block the batch.

import { createHash } from "node:crypto";
import type { Kysely } from "kysely";
import webpush, { WebPushError } from "web-push";
import { log } from "@roost/shared/log";
import type { DB, PushSubscriptionsTable } from "./db/schema.ts";
import { getVapidKeys } from "./vapid.ts";

const TTL_SECONDS = 60;
const REQUEST_TIMEOUT_MS = 10_000;
const MAX_CONCURRENT_SENDS = 4;
let configuredPublicKey: string | undefined;
let configurePromise: Promise<void> | undefined;

function endpointId(endpoint: string): string {
  return createHash("sha256").update(endpoint).digest("hex").slice(0, 12);
}

async function ensureConfigured(db: Kysely<DB>): Promise<void> {
  if (configuredPublicKey) return;
  if (!configurePromise) {
    configurePromise = (async () => {
      const keys = await getVapidKeys(db);
      webpush.setVapidDetails("mailto:roost@local", keys.publicKey, keys.privateKey);
      configuredPublicKey = keys.publicKey;
    })();
  }
  try {
    await configurePromise;
  } catch (error) {
    configurePromise = undefined;
    throw error;
  }
}

export interface PushDeliveryResult {
  delivered: number;
  expired: number;
  failed: number;
}

export type PushNotificationTransport = Pick<typeof webpush, "sendNotification">;

function pushStatusCode(error: unknown): number | null {
  if (error instanceof WebPushError) return error.statusCode;
  if (
    typeof error === "object"
    && error !== null
    && "statusCode" in error
    && typeof error.statusCode === "number"
  ) return error.statusCode;
  return null;
}

/** Encrypt and deliver one payload to a bounded set of stored subscriptions. */
export async function sendPushToSubscriptions(
  db: Kysely<DB>,
  subscriptions: PushSubscriptionsTable[],
  payload: object,
  transport: PushNotificationTransport = webpush,
): Promise<PushDeliveryResult> {
  if (subscriptions.length === 0) return { delivered: 0, expired: 0, failed: 0 };
  try {
    await ensureConfigured(db);
  } catch (error) {
    log.warn("push", "vapid_config_failed", { error: String(error) });
    return { delivered: 0, expired: 0, failed: subscriptions.length };
  }

  const body = JSON.stringify(payload);
  let nextIndex = 0;
  let delivered = 0;
  let expired = 0;
  let failed = 0;
  const workers = Array.from(
    { length: Math.min(MAX_CONCURRENT_SENDS, subscriptions.length) },
    async () => {
      for (;;) {
        const index = nextIndex++;
        if (index >= subscriptions.length) return;
        const subscription = subscriptions[index]!;
        const endpoint = subscription.endpoint;
        try {
          // web-push issues one node:https request and rejects every non-2xx
          // response; there is deliberately no redirect replay.
          await transport.sendNotification(
            {
              endpoint,
              keys: { p256dh: subscription.p256dh, auth: subscription.auth },
            },
            body,
            { TTL: TTL_SECONDS, timeout: REQUEST_TIMEOUT_MS },
          );
          delivered++;
        } catch (error) {
          const status = pushStatusCode(error);
          if (status === 404 || status === 410) {
            await db
              .deleteFrom("push_subscriptions")
              .where("viewer_fp", "=", subscription.viewer_fp)
              .where("dashboard_id", "=", subscription.dashboard_id)
              .where("endpoint", "=", endpoint)
              .execute();
            log.info("push", "subscription_expired", {
              endpoint_id: endpointId(endpoint),
              status,
            });
            expired++;
            continue;
          }
          log.warn("push", "send_failed", {
            endpoint_id: endpointId(endpoint),
            status,
            error: String(error),
          });
          failed++;
        }
      }
    },
  );
  await Promise.all(workers);
  return { delivered, expired, failed };
}
