// Web Push VAPID key management. The P-256 identity is coordinator-global:
// it lives in the explicit NULL dashboard scope, never in a tenant setting.
// Push handlers and delivery share this cache so first-use key generation stays serialized.

import type { Kysely } from "kysely";
import webpush from "web-push";
import type { DB } from "./db/schema.ts";

const VAPID_SETTING_KEY = "push.vapid";

export interface VapidKeys {
  publicKey: string;
  privateKey: string;
}

let cachedKeys: VapidKeys | undefined;
let loadPromise: Promise<VapidKeys> | undefined;

function parseVapidKeys(value: string): VapidKeys {
  const parsed = JSON.parse(value) as Partial<VapidKeys>;
  if (typeof parsed.publicKey !== "string" || typeof parsed.privateKey !== "string") {
    throw new Error("stored VAPID keypair is invalid");
  }
  return { publicKey: parsed.publicKey, privateKey: parsed.privateKey };
}

async function loadOrCreateVapidKeys(db: Kysely<DB>): Promise<VapidKeys> {
  const row = await db
    .selectFrom("app_settings")
    .select("value")
    .where("dashboard_id", "is", null)
    .where("key", "=", VAPID_SETTING_KEY)
    .executeTakeFirst();
  if (row) return parseVapidKeys(row.value);

  const generated = webpush.generateVAPIDKeys();
  const keys = { publicKey: generated.publicKey, privateKey: generated.privateKey };
  const value = JSON.stringify(keys);
  const now = Date.now();
  await db
    .insertInto("app_settings")
    .values({ dashboard_id: null, key: VAPID_SETTING_KEY, value, updated_at_ms: now })
    .execute();

  // `loadPromise` serializes first use in the one coordinator process. During
  // a handoff, read the retained coordinator-global row rather than a tenant
  // setting; deployment guarantees only one writer after cutover.
  const persisted = await db
    .selectFrom("app_settings")
    .select("value")
    .where("dashboard_id", "is", null)
    .where("key", "=", VAPID_SETTING_KEY)
    .executeTakeFirstOrThrow();
  return parseVapidKeys(persisted.value);
}

/** Load the persisted VAPID identity, serializing first-use generation. */
export async function getVapidKeys(db: Kysely<DB>): Promise<VapidKeys> {
  if (cachedKeys) return cachedKeys;
  if (!loadPromise) {
    loadPromise = loadOrCreateVapidKeys(db).then((keys) => {
      cachedKeys = keys;
      return keys;
    });
  }
  try {
    return await loadPromise;
  } catch (error) {
    loadPromise = undefined;
    throw error;
  }
}

/** Test seam for independent temporary databases. */
export function resetVapidKeysForTest(): void {
  cachedKeys = undefined;
  loadPromise = undefined;
}
