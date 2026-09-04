// Covers active-owner topology and dashboard scoping for managed-container startup.
// Bun discovers this suite directly and calls the shared prefixed database fixture.
// The invariant API and migrated coord schema define its accepted and rejected states.
import { describe, expect, test } from "bun:test";
import {
  assertManagedContainerInvariant,
  MANAGED_RUNTIME_SCOPE_TABLES,
} from "../src/managed-container-invariant.ts";
import {
  INSTANCE_ID,
  MANAGED_CONFIG,
  NOW,
  OTHER_ACCOUNT_ID,
  OTHER_INSTANCE_ID,
  insertRuntimeRow,
  migratedDatabase,
  seedActiveOwner,
  withoutForeignKeys,
} from "./managed-container-invariant-database-fixture.ts";

describe("managed-container startup invariant", () => {
  test("accepts the exact active owner and fully scoped runtime on restart", async () => {
    const sqlite = await migratedDatabase();
    seedActiveOwner(sqlite);
    for (const table of MANAGED_RUNTIME_SCOPE_TABLES) {
      insertRuntimeRow(sqlite, table, INSTANCE_ID);
    }
    sqlite.query(`
      INSERT INTO audit_log (
        dashboard_id, ts, caller_fp, method, path, status, trace_id
      ) VALUES (NULL, ?, NULL, 'GET', '/api/unknown', 404, NULL)
    `).run(NOW);
    sqlite.query(`
      INSERT INTO app_settings (dashboard_id, key, value, updated_at_ms)
      VALUES (NULL, 'push.vapid', '{}', ?)
    `).run(NOW);
    expect(() => assertManagedContainerInvariant(sqlite, MANAGED_CONFIG, NOW)).not.toThrow();
    expect(() => assertManagedContainerInvariant(sqlite, MANAGED_CONFIG, NOW)).not.toThrow();
  });

  for (const [table, deletion] of [
    ["accounts", "DELETE FROM accounts"],
    ["owner activation", "DELETE FROM owner_activation_tokens"],
    ["account identity", "DELETE FROM account_identities"],
    ["organization", "DELETE FROM organizations"],
    ["organization membership", "DELETE FROM organization_memberships"],
    ["dashboard", "DELETE FROM dashboards"],
    ["dashboard membership", "DELETE FROM dashboard_memberships"],
  ] as const) {
    test(`rejects a missing ${table}`, async () => {
      const sqlite = await migratedDatabase();
      seedActiveOwner(sqlite);
      withoutForeignKeys(sqlite, () => sqlite.exec(deletion));
      expect(() => assertManagedContainerInvariant(sqlite, MANAGED_CONFIG, NOW))
        .toThrow("managed-container invariant violation");
    });
  }

  for (const [table, insertion] of [
    ["account", `INSERT INTO accounts VALUES (
      '${OTHER_ACCOUNT_ID}', 'other@example.test', 'hash', 'active', ${NOW}, ${NOW}
    )`],
    ["owner activation", `INSERT INTO owner_activation_tokens VALUES (
      '${OTHER_INSTANCE_ID}', '${OTHER_ACCOUNT_ID}', 'other@example.test',
      'other-hash', 'activation-outbox', 'coordinator-email',
      ${NOW}, ${NOW + 60_000}, ${NOW}, NULL
    )`],
    ["account identity", `INSERT INTO account_identities VALUES (
      '${OTHER_ACCOUNT_ID}', 'native', '${OTHER_ACCOUNT_ID}',
      'other@example.test', ${NOW}, NULL, NULL
    )`],
    ["organization", `INSERT INTO organizations VALUES (
      '${OTHER_ACCOUNT_ID}', 'other', 'Other', 'active', ${NOW}
    )`],
    ["organization membership", `INSERT INTO organization_memberships VALUES (
      '${OTHER_ACCOUNT_ID}', '${OTHER_ACCOUNT_ID}', 'owner', ${NOW}
    )`],
    ["dashboard", `INSERT INTO dashboards VALUES (
      '${OTHER_INSTANCE_ID}', '${OTHER_ACCOUNT_ID}', 'default', 'Personal', 'active', ${NOW}
    )`],
    ["dashboard membership", `INSERT INTO dashboard_memberships VALUES (
      '${OTHER_INSTANCE_ID}', '${OTHER_ACCOUNT_ID}', 'admin', ${NOW}
    )`],
  ] as const) {
    test(`rejects an extra ${table}`, async () => {
      const sqlite = await migratedDatabase();
      seedActiveOwner(sqlite);
      withoutForeignKeys(sqlite, () => sqlite.exec(insertion));
      expect(() => assertManagedContainerInvariant(sqlite, MANAGED_CONFIG, NOW))
        .toThrow("managed-container invariant violation");
    });
  }

  for (const [caseName, mutation] of [
    ["disabled account", "UPDATE accounts SET status = 'disabled'"],
    ["missing password", "UPDATE accounts SET password_hash = NULL"],
    ["unaccepted activation", "UPDATE owner_activation_tokens SET accepted_at_ms = NULL"],
    ["revoked activation", `UPDATE owner_activation_tokens SET revoked_at_ms = ${NOW}`],
    ["cross-account activation", `UPDATE owner_activation_tokens SET account_id = '${OTHER_ACCOUNT_ID}'`],
    ["revoked identity", `UPDATE account_identities SET revoked_at_ms = ${NOW}`],
    ["non-native identity", "UPDATE account_identities SET issuer = 'oidc'"],
    ["cross-account identity", `UPDATE account_identities SET account_id = '${OTHER_ACCOUNT_ID}'`],
    ["cross-subject identity", `UPDATE account_identities SET subject = '${OTHER_ACCOUNT_ID}'`],
    ["wrong organization ID", `UPDATE organizations SET id = '${OTHER_ACCOUNT_ID}'`],
    ["wrong organization slug", "UPDATE organizations SET slug = 'shared'"],
    ["wrong organization name", "UPDATE organizations SET name = 'Other'"],
    ["suspended organization", "UPDATE organizations SET status = 'suspended'"],
    ["cross-organization membership", `UPDATE organization_memberships SET organization_id = '${OTHER_ACCOUNT_ID}'`],
    ["cross-account organization membership", `UPDATE organization_memberships SET account_id = '${OTHER_ACCOUNT_ID}'`],
    ["wrong organization role", "UPDATE organization_memberships SET role = 'admin'"],
    ["wrong dashboard ID", `UPDATE dashboards SET id = '${OTHER_INSTANCE_ID}'`],
    ["cross-organization dashboard", `UPDATE dashboards SET organization_id = '${OTHER_ACCOUNT_ID}'`],
    ["wrong dashboard slug", "UPDATE dashboards SET slug = 'other'"],
    ["wrong dashboard name", "UPDATE dashboards SET name = 'Other'"],
    ["suspended dashboard", "UPDATE dashboards SET status = 'suspended'"],
    ["cross-dashboard membership", `UPDATE dashboard_memberships SET dashboard_id = '${OTHER_INSTANCE_ID}'`],
    ["cross-account dashboard membership", `UPDATE dashboard_memberships SET account_id = '${OTHER_ACCOUNT_ID}'`],
    ["wrong dashboard role", "UPDATE dashboard_memberships SET role = 'member'"],
    ["cross-account device", `UPDATE account_devices SET account_id = '${OTHER_ACCOUNT_ID}'`],
  ] as const) {
    test(`rejects ${caseName}`, async () => {
      const sqlite = await migratedDatabase();
      seedActiveOwner(sqlite);
      withoutForeignKeys(sqlite, () => sqlite.exec(mutation));
      expect(() => assertManagedContainerInvariant(sqlite, MANAGED_CONFIG, NOW))
        .toThrow("managed-container invariant violation");
    });
  }

  for (const table of [
    "workers",
    "bootstrap_tokens",
    "events",
    "sessions",
    "workspaces",
    "workspace_sessions",
    "tasks",
    "mcp_relays",
    "push_subscriptions",
  ] as const) {
    test(`rejects invalid ${table} scope at the SQL boundary`, async () => {
      for (const dashboardId of [null, OTHER_INSTANCE_ID]) {
        const sqlite = await migratedDatabase();
        seedActiveOwner(sqlite);
        expect(
          () => insertRuntimeRow(sqlite, table, dashboardId, false),
        ).toThrow();
      }
    });
  }

  for (const table of [
    "audit_log",
    "app_settings",
  ] as const) {
    test(`confines every ${table} row to its allowed dashboard scope`, async () => {
      if (table === "audit_log") {
        const sqlite = await migratedDatabase();
        seedActiveOwner(sqlite);
        insertRuntimeRow(sqlite, table, null);
        expect(
          () => assertManagedContainerInvariant(sqlite, MANAGED_CONFIG, NOW),
        ).not.toThrow();
        insertRuntimeRow(sqlite, table, OTHER_INSTANCE_ID);
        expect(() => assertManagedContainerInvariant(sqlite, MANAGED_CONFIG, NOW))
          .toThrow(`${table} contains a row outside the allowed dashboard scope`);
        return;
      }

      for (const dashboardId of [null, OTHER_INSTANCE_ID]) {
        const sqlite = await migratedDatabase();
        seedActiveOwner(sqlite);
        insertRuntimeRow(sqlite, table, dashboardId);
        expect(
          () => assertManagedContainerInvariant(sqlite, MANAGED_CONFIG, NOW),
        ).toThrow(`${table} contains a row outside the allowed dashboard scope`);
      }
    });
  }

  test("reserves the nullable app setting scope exclusively for VAPID", async () => {
    const sqlite = await migratedDatabase();
    seedActiveOwner(sqlite);
    sqlite.query(`
      INSERT INTO app_settings (dashboard_id, key, value, updated_at_ms)
      VALUES (NULL, 'push.vapid', '{}', ?)
    `).run(NOW);
    expect(() => assertManagedContainerInvariant(sqlite, MANAGED_CONFIG, NOW)).not.toThrow();
    sqlite.query(
      "UPDATE app_settings SET dashboard_id = ? WHERE key = 'push.vapid'",
    ).run(INSTANCE_ID);
    expect(() => assertManagedContainerInvariant(sqlite, MANAGED_CONFIG, NOW))
      .toThrow("app_settings contains a row outside the allowed dashboard scope");
  });
});
