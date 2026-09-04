/**
 * Owns bootstrap-token schema, row conversion, relocation, and rejection coverage.
 * Bun discovers these migration cases while scope-trigger coverage runs separately.
 * It depends on shared pre-0024 fixtures and the self-hosted tenant validator.
 */
import { createHash } from "node:crypto";
import { describe, expect, test } from "bun:test";
import { ensureSelfHostedTenant } from "../src/self-hosted-tenant.ts";
import {
  addAccountDevice,
  addDashboardMember,
  apply0024,
  expectSqlFailure,
  insertLegacyToken,
  openPre0024,
  seedTenant,
} from "./bootstrap-token-migration-fixtures.ts";

function digest(value: string): string {
  return createHash("sha256").update(value, "utf8").digest("hex");
}

describe("0024 auth and tenancy stabilization migration", () => {
  test("applies to a fresh database and installs the exact digest and relocation schema", async () => {
    const sqlite = await openPre0024();
    try {
      await apply0024(sqlite);

      const bootstrapColumns = sqlite.query(`
        SELECT name, type, "notnull" AS is_not_null, pk
        FROM pragma_table_info('bootstrap_tokens')
        ORDER BY cid
      `).all();
      expect(bootstrapColumns).toEqual([
        { name: "token_hash", type: "TEXT", is_not_null: 1, pk: 1 },
        { name: "account_id", type: "TEXT", is_not_null: 1, pk: 0 },
        { name: "dashboard_id", type: "TEXT", is_not_null: 1, pk: 0 },
        { name: "kind", type: "TEXT", is_not_null: 1, pk: 0 },
        { name: "label", type: "TEXT", is_not_null: 1, pk: 0 },
        { name: "created_at_ms", type: "INTEGER", is_not_null: 1, pk: 0 },
        { name: "expires_at_ms", type: "INTEGER", is_not_null: 1, pk: 0 },
        { name: "used_at_ms", type: "INTEGER", is_not_null: 0, pk: 0 },
        { name: "used_by_fp", type: "TEXT", is_not_null: 0, pk: 0 },
        { name: "minted_by_fp", type: "TEXT", is_not_null: 0, pk: 0 },
      ]);
      expect(sqlite.query(`
        SELECT name, type, "notnull" AS is_not_null, pk
        FROM pragma_table_info('coordinator_relocation_redemptions')
        ORDER BY cid
      `).all()).toEqual([
        { name: "jti", type: "TEXT", is_not_null: 1, pk: 1 },
        { name: "account_id", type: "TEXT", is_not_null: 1, pk: 0 },
        { name: "redeemed_at_ms", type: "INTEGER", is_not_null: 1, pk: 0 },
        { name: "expires_at_ms", type: "INTEGER", is_not_null: 1, pk: 0 },
        { name: "used_by_fp", type: "TEXT", is_not_null: 1, pk: 0 },
        { name: "delegated_by_fp", type: "TEXT", is_not_null: 1, pk: 0 },
      ]);
      expect(sqlite.query(`
        SELECT name FROM sqlite_master
        WHERE type = 'index' AND name = 'bootstrap_tokens_dashboard_idx'
      `).get()).toEqual({ name: "bootstrap_tokens_dashboard_idx" });
      expect(sqlite.query(`
        SELECT name FROM sqlite_master
        WHERE type = 'trigger' AND name = 'bootstrap_tokens_reject_revoked_minter'
      `).get()).toEqual({ name: "bootstrap_tokens_reject_revoked_minter" });
    } finally {
      sqlite.close(true);
    }
  });

  test("hashes unused, used, and expired ordinary grants without retaining plaintext", async () => {
    const sqlite = await openPre0024();
    const tenant = seedTenant(sqlite, "ordinary");
    const future = Date.now() + 60_000;
    const rows = [
      {
        token: "unused-plaintext-grant",
        label: "Unused grant",
        expiresAtMs: future,
        usedAtMs: null,
        usedByFp: null,
      },
      {
        token: "used-plaintext-grant",
        label: "Used grant",
        expiresAtMs: future,
        usedAtMs: 123,
        usedByFp: "same-key-fingerprint",
      },
      {
        token: "expired-plaintext-grant",
        label: "Expired grant",
        expiresAtMs: 2,
        usedAtMs: null,
        usedByFp: null,
      },
    ];
    try {
      for (const row of rows) {
        insertLegacyToken(sqlite, {
          ...row,
          dashboardId: tenant.dashboardId,
          createdAtMs: 1,
          label: row.label,
        });
      }

      await apply0024(sqlite);

      expect(sqlite.query(`
        SELECT token_hash, account_id, dashboard_id, label, created_at_ms,
               expires_at_ms, used_at_ms, used_by_fp, minted_by_fp
        FROM bootstrap_tokens
        ORDER BY label
      `).all()).toEqual(rows
        .map((row) => ({
          token_hash: digest(row.token),
          account_id: tenant.accountId,
          dashboard_id: tenant.dashboardId,
          label: row.label,
          created_at_ms: 1,
          expires_at_ms: row.expiresAtMs,
          used_at_ms: row.usedAtMs,
          used_by_fp: row.usedByFp,
          minted_by_fp: null,
        }))
        .sort((left, right) => left.label.localeCompare(right.label)));
      expect((sqlite.query("PRAGMA table_info(bootstrap_tokens)").all() as Array<{
        name: string;
      }>).map(({ name }) => name)).not.toContain("token");
      expect(sqlite.query(`
        SELECT name FROM sqlite_master
        WHERE name = 'bootstrap_tokens_plaintext_0024'
      `).get()).toBeNull();
      const logicalRows = JSON.stringify(
        sqlite.query("SELECT * FROM bootstrap_tokens").all(),
      );
      for (const { token } of rows) expect(logicalRows).not.toContain(token);

      expectSqlFailure(sqlite, `
        INSERT INTO bootstrap_tokens
          (token_hash, account_id, dashboard_id, kind, label, created_at_ms,
           expires_at_ms, used_at_ms, used_by_fp, minted_by_fp)
        VALUES ('${"A".repeat(64)}', '${tenant.accountId}',
                '${tenant.dashboardId}', 'browser', 'bad', 1, 2, NULL, NULL, NULL)
      `);
      expectSqlFailure(sqlite, `
        INSERT INTO bootstrap_tokens
          (token_hash, account_id, dashboard_id, kind, label, created_at_ms,
           expires_at_ms, used_at_ms, used_by_fp, minted_by_fp)
        VALUES ('${"0".repeat(64)}', '${tenant.accountId}',
                '${tenant.dashboardId}', 'other', 'bad', 1, 2, NULL, NULL, NULL)
      `);
    } finally {
      sqlite.close(true);
    }
  });

  test("post-0024 self-hosted validation rejects a foreign grant account", async () => {
    const sqlite = await openPre0024();
    const tenant = seedTenant(sqlite, "foreign-grant-account");
    try {
      await apply0024(sqlite);
      sqlite.exec("PRAGMA foreign_keys = OFF");
      sqlite.query(`
        INSERT INTO bootstrap_tokens
          (token_hash, account_id, dashboard_id, kind, label, created_at_ms,
           expires_at_ms, used_at_ms, used_by_fp, minted_by_fp)
        VALUES (?, 'foreign-account', ?, 'browser', 'foreign', 1, 2,
                NULL, NULL, NULL)
      `).run("2".repeat(64), tenant.dashboardId);
      sqlite.exec("PRAGMA foreign_keys = ON");

      expect(() => ensureSelfHostedTenant(sqlite, {
        backfillLegacyScopes: false,
      })).toThrow(
        "self-hosted tenant invariant violation: "
          + "bootstrap_tokens contains missing or foreign account scope",
      );
    } finally {
      sqlite.close(true);
    }
  });

  test("converts only unexpired relocation sentinels with live delegator authority", async () => {
    const sqlite = await openPre0024();
    const active = seedTenant(sqlite, "relocation-active");
    const inactive = seedTenant(sqlite, "relocation-inactive", "disabled");
    const future = Date.now() + 60_000;
    const eligibleFp = "eligible-delegator";
    const revokedFp = "revoked-delegator";
    const inactiveFp = "inactive-delegator";
    try {
      addAccountDevice(sqlite, eligibleFp, active.accountId);
      addAccountDevice(sqlite, revokedFp, active.accountId);
      addAccountDevice(sqlite, inactiveFp, inactive.accountId);

      insertLegacyToken(sqlite, {
        token: "roost_move_eligible-jti",
        dashboardId: null,
        expiresAtMs: future,
        usedAtMs: 101,
        usedByFp: "destination-fp",
        mintedByFp: eligibleFp,
      });
      insertLegacyToken(sqlite, {
        token: "roost_move_expired-jti",
        dashboardId: null,
        expiresAtMs: 1,
        usedAtMs: 102,
        usedByFp: "destination-fp",
        mintedByFp: eligibleFp,
      });
      insertLegacyToken(sqlite, {
        token: "roost_move_revoked-jti",
        dashboardId: null,
        expiresAtMs: future,
        usedAtMs: 103,
        usedByFp: "destination-fp",
        mintedByFp: revokedFp,
      });
      insertLegacyToken(sqlite, {
        token: "roost_move_inactive-jti",
        dashboardId: null,
        expiresAtMs: future,
        usedAtMs: 104,
        usedByFp: "destination-fp",
        mintedByFp: inactiveFp,
      });
      insertLegacyToken(sqlite, {
        token: "roost_move_missing-jti",
        dashboardId: null,
        expiresAtMs: future,
        usedAtMs: 105,
        usedByFp: "destination-fp",
        mintedByFp: "missing-delegator",
      });
      insertLegacyToken(sqlite, {
        token: "roost_move_unused-jti",
        dashboardId: null,
        expiresAtMs: future,
        usedAtMs: null,
        usedByFp: null,
        mintedByFp: eligibleFp,
      });
      sqlite.query(`
        INSERT INTO authorized_key_revocations
          (fingerprint, revoked_at_ms, revoked_by_fp, reason)
        VALUES (?, 2, 'test', 'test')
      `).run(revokedFp);

      await apply0024(sqlite);

      expect(sqlite.query(`
        SELECT jti, account_id, redeemed_at_ms, expires_at_ms, used_by_fp,
               delegated_by_fp
        FROM coordinator_relocation_redemptions
      `).all()).toEqual([{
        jti: "eligible-jti",
        account_id: active.accountId,
        redeemed_at_ms: 101,
        expires_at_ms: future,
        used_by_fp: "destination-fp",
        delegated_by_fp: eligibleFp,
      }]);
      expect(sqlite.query("SELECT * FROM bootstrap_tokens").all()).toEqual([]);
    } finally {
      sqlite.close(true);
    }
  });

  test("rolls back instead of guessing ordinary authority on an ambiguous dashboard", async () => {
    const sqlite = await openPre0024();
    const tenant = seedTenant(sqlite, "ambiguous");
    addDashboardMember(
      sqlite,
      tenant.dashboardId,
      tenant.organizationId,
      "ambiguous-peer",
    );
    insertLegacyToken(sqlite, {
      token: "must-remain-plaintext-after-rollback",
      dashboardId: tenant.dashboardId,
      expiresAtMs: Date.now() + 60_000,
    });
    try {
      await expect(apply0024(sqlite)).rejects.toThrow("migration failed");
      expect(sqlite.query(`
        SELECT token FROM bootstrap_tokens
      `).all()).toEqual([{ token: "must-remain-plaintext-after-rollback" }]);
      expect(sqlite.query(`
        SELECT name FROM _migrations
        WHERE name = '0024_auth_tenancy_stabilization'
      `).get()).toBeNull();
      expect(sqlite.query(`
        SELECT name FROM sqlite_master
        WHERE name = 'coordinator_relocation_redemptions'
      `).get()).toBeNull();
      expect(sqlite.query(`
        SELECT name FROM sqlite_temp_master
        WHERE name = '_roost_sha256_hex_0024'
      `).get()).toBeNull();
    } finally {
      sqlite.close(true);
    }
  });

  test("rejects non-text and oversized inputs to the migration-only digest", async () => {
    for (const token of [new Uint8Array([1, 2, 3]), "x".repeat(1_000_000)]) {
      const sqlite = await openPre0024();
      const tenant = seedTenant(sqlite, `bounded-${typeof token === "string" ? "large" : "blob"}`);
      try {
        insertLegacyToken(sqlite, {
          token,
          dashboardId: tenant.dashboardId,
          expiresAtMs: Date.now() + 60_000,
        });
        await expect(apply0024(sqlite)).rejects.toThrow("migration failed");
        expect(sqlite.query("SELECT token FROM bootstrap_tokens").get()).not.toBeNull();
        expect(sqlite.query(`
          SELECT name FROM _migrations
          WHERE name = '0024_auth_tenancy_stabilization'
        `).get()).toBeNull();
        expect(sqlite.query(`
          SELECT name FROM sqlite_temp_master
          WHERE name = '_roost_sha256_hex_0024'
        `).get()).toBeNull();
      } finally {
        sqlite.close(true);
      }
    }
  });

});
