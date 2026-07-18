// VAPID key management for Web Push. Generates a P-256 keypair on first use and
// persists it in app_settings under "push.vapid" (JSON blob) so every device
// shares one server identity across restarts. Keys are cached in-memory after
// first load. web-push owns the VAPID JWT signing (see push-sender.ts); this
// module only sources the keypair.
//
// The public key is the base64url raw 65-byte uncompressed P-256 point — the
// exact value both the browser's applicationServerKey and web-push's
// setVapidDetails expect.

import type { Kysely } from "kysely";
import type { DB } from "./db/schema.ts";
import webpush from "web-push";

const KEY = "push.vapid";

export interface VapidKeys {
  publicKey: string; // base64url raw 65-byte P-256 point
  privateKey: string; // base64url raw 32-byte private scalar
}

let _cache: VapidKeys | null = null;

/** Load the persisted VAPID keypair, generating + storing one on first use. */
export async function getVapidKeys(db: Kysely<DB>): Promise<VapidKeys> {
  if (_cache) return _cache;

  const row = await db
    .selectFrom("app_settings")
    .select("value")
    .where("key", "=", KEY)
    .executeTakeFirst();

  if (row) {
    _cache = JSON.parse(row.value) as VapidKeys;
    return _cache;
  }

  const keys = webpush.generateVAPIDKeys();
  const value = JSON.stringify(keys);
  const now = Date.now();
  await db
    .insertInto("app_settings")
    .values({ key: KEY, value, updated_at_ms: now })
    .onConflict((oc) => oc.column("key").doUpdateSet({ value, updated_at_ms: now }))
    .execute();

  _cache = keys;
  return keys;
}
