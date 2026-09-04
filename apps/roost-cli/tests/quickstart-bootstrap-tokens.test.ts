import { Database } from "bun:sqlite";
import { expect, test } from "bun:test";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { bootstrapTokenDigest } from "../../coord/src/bootstrap-tokens.ts";
import { runMigrations } from "../../coord/src/db/migrate.ts";
import { ensureSelfHostedTenant } from "../../coord/src/self-hosted-tenant.ts";
import {
  mintBrowserToken,
  mintWorkerToken,
} from "../src/quickstart-bootstrap-tokens.ts";

test("host quickstart minting stores scoped digests and no plaintext bearer", async () => {
  const root = await mkdtemp(join(tmpdir(), "roost-quickstart-token-"));
  const databasePath = join(root, "coordinator_v2.db");
  const sqlite = new Database(databasePath);
  try {
    await runMigrations(sqlite, undefined, undefined, (name) => {
      if (name === "0024_auth_tenancy_stabilization") {
        ensureSelfHostedTenant(sqlite, { backfillLegacyScopes: true });
      }
    });
    ensureSelfHostedTenant(sqlite, { backfillLegacyScopes: false });
  } finally {
    sqlite.close(true);
  }

  try {
    const workerToken = await mintWorkerToken(databasePath, "quickstart-worker");
    const browserToken = await mintBrowserToken(databasePath, "quickstart-browser");
    const verify = new Database(databasePath, { readonly: true, strict: true });
    try {
      const rows = verify.query(`
        SELECT token_hash, account_id, dashboard_id, kind, label, minted_by_fp
        FROM bootstrap_tokens
        ORDER BY label
      `).all() as Array<{
        token_hash: string;
        account_id: string;
        dashboard_id: string;
        kind: string;
        label: string;
        minted_by_fp: string | null;
      }>;
      expect(rows).toHaveLength(2);
      expect(rows.map((row) => row.kind)).toEqual(["browser", "worker"]);
      expect(rows.every((row) => row.account_id.length > 0)).toBe(true);
      expect(rows.every((row) => row.dashboard_id.length > 0)).toBe(true);
      expect(new Set(rows.map((row) => row.account_id)).size).toBe(1);
      expect(new Set(rows.map((row) => row.dashboard_id)).size).toBe(1);
      expect(rows.every((row) => row.minted_by_fp === null)).toBe(true);
      expect(rows.find((row) => row.kind === "worker")?.token_hash)
        .toBe(await bootstrapTokenDigest(workerToken));
      expect(rows.find((row) => row.kind === "browser")?.token_hash)
        .toBe(await bootstrapTokenDigest(browserToken));
      expect(rows.some((row) => row.token_hash === workerToken || row.token_hash === browserToken))
        .toBe(false);
      const columns = verify.query("PRAGMA table_info(bootstrap_tokens)").all() as Array<{ name: string }>;
      expect(columns.map((column) => column.name)).not.toContain("token");
    } finally {
      verify.close(true);
    }
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});
