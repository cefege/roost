// Self-hosted browser authorization is shared by bootstrap redemption and pairing.
// Keeping its managed-mode guard and account association together prevents either
// public enrollment path from accidentally admitting legacy browser authority.

import { Code, ConnectError } from "@connectrpc/connect";
import type { KyselyDB } from "../db/connection.ts";
import type { ConnectDeps } from "./router.ts";

export function rejectManagedLegacyBrowserAuth(deps: ConnectDeps): void {
  if (deps.cfg.saasMode) {
    throw new ConnectError(
      "legacy browser authorization is unavailable in managed mode",
      Code.PermissionDenied,
    );
  }
}

export async function selfHostedBrowserAccountId(
  db: KyselyDB,
  authorityFingerprint?: string,
): Promise<string | null> {
  if (authorityFingerprint) {
    const device = await db.selectFrom("account_devices")
      .select("account_id")
      .where("fingerprint", "=", authorityFingerprint)
      .executeTakeFirst();
    if (device) return device.account_id;
  }
  const accounts = await db.selectFrom("accounts")
    .select(["id", "status"])
    .limit(2)
    .execute();
  return accounts.length === 1 && accounts[0]?.status === "active"
    ? accounts[0].id
    : null;
}

export async function associateSelfHostedBrowser(
  db: KyselyDB,
  fingerprint: string,
  accountId: string | null,
  now: number,
): Promise<void> {
  if (accountId === null) return;
  const worker = await db.selectFrom("workers").select("fp")
    .where("fp", "=", fingerprint).executeTakeFirst();
  const device = await db.selectFrom("account_devices").select("account_id")
    .where("fingerprint", "=", fingerprint).executeTakeFirst();
  if (worker) {
    throw new ConnectError("device key is already in use by a worker", Code.AlreadyExists);
  }
  if (device && device.account_id !== accountId) {
    throw new ConnectError("device already belongs to another account", Code.AlreadyExists);
  }
  await db.insertInto("account_devices").values({
    fingerprint,
    account_id: accountId,
    added_at_ms: now,
    last_seen_at_ms: now,
  }).onConflict((conflict) => conflict.column("fingerprint").doUpdateSet({
    last_seen_at_ms: now,
  })).execute();
}
