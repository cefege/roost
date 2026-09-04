// Host-local quickstart grants. The coordinator token owner resolves the sole
// self-hosted tenant, persists only the digest, and returns the live bearer.
// Both source and compiled quickstart paths use this same digest-only boundary.

import { Database } from "bun:sqlite";
import {
  bootstrapTokenDigest,
  mintHostBootstrapToken,
} from "../../coord/src/bootstrap-tokens.ts";

/** Browser grant redeemed by the host browser from the #pair fragment. */
export async function mintBrowserToken(
  databasePath: string,
  label: string,
): Promise<string> {
  return (await mintHostBootstrapToken(databasePath, {
    kind: "browser",
    label,
  })).token;
}

/** Worker grant redeemed by the local worker during quickstart enrollment. */
export async function mintWorkerToken(
  databasePath: string,
  label: string,
): Promise<string> {
  return (await mintHostBootstrapToken(databasePath, {
    kind: "worker",
    label,
  })).token;
}

/**
 * Prove that this exact worker grant was claimed and produced the scoped worker
 * principal. The bearer is digested before SQLite is opened and is never used
 * in SQL, logs, or errors.
 */
export async function registeredWorkerForGrant(
  databasePath: string,
  token: string,
): Promise<string | null> {
  const tokenHash = await bootstrapTokenDigest(token);
  const sqlite = new Database(databasePath, { readonly: true, strict: true });
  try {
    const row = sqlite.query(`
      SELECT bt.used_by_fp AS fingerprint
      FROM bootstrap_tokens AS bt
      JOIN workers AS worker
        ON worker.fp = bt.used_by_fp
       AND worker.dashboard_id = bt.dashboard_id
      JOIN authorized_keys AS authorized_key
        ON authorized_key.fingerprint = worker.fp
      WHERE bt.token_hash = ?
        AND bt.kind = 'worker'
        AND bt.used_at_ms IS NOT NULL
        AND bt.used_by_fp IS NOT NULL
      LIMIT 1
    `).get(tokenHash) as { fingerprint: string } | null;
    return row?.fingerprint ?? null;
  } finally {
    sqlite.close(false);
  }
}
