// These tests own self-hosted tenant creation, reuse, validation, and rollback.
// The startup path calls the tenant initializer against migrated coord databases.
// They depend on shared legacy database fixtures and in-memory SQLite.
// Migration-hook ordering is covered by its focused sibling suite.
import { describe, expect, test } from "bun:test";
import { Database } from "bun:sqlite";
import { ensureSelfHostedTenant } from "../src/self-hosted-tenant.ts";
import {
  captureInvariantError,
  dashboardIds,
  insertAuthorizedKey,
  insertWorker,
  INVARIANT_PREFIX,
  migratedPre0024Database,
  seedExistingTopology,
  seedLegacyRuntimeRows,
  STRICT_SCOPE_TABLES,
} from "./self-hosted-tenant-fixtures.ts";


describe("self-hosted tenant initialization", () => {
  test("creates and atomically scopes a fresh legacy self-hosted database", async () => {
    const sqlite = await migratedPre0024Database();
    try {
      seedLegacyRuntimeRows(sqlite);
      const tenant = ensureSelfHostedTenant(sqlite, {
        backfillLegacyScopes: true,
      });

      expect(sqlite.query(`
        SELECT id, email_normalized, password_hash, status, password_changed_at_ms
        FROM accounts
      `).all()).toEqual([{
        id: tenant.accountId,
        email_normalized: "local@roost.invalid",
        password_hash: null,
        status: "active",
        password_changed_at_ms: null,
      }]);
      expect(sqlite.query("SELECT id, slug, name, status FROM organizations").all())
        .toEqual([{
          id: tenant.organizationId,
          slug: "personal",
          name: "Personal",
          status: "active",
        }]);
      expect(sqlite.query(`
        SELECT id, organization_id, slug, name, status FROM dashboards
      `).all()).toEqual([{
        id: tenant.dashboardId,
        organization_id: tenant.organizationId,
        slug: "default",
        name: "Personal",
        status: "active",
      }]);
      expect(sqlite.query("SELECT role FROM organization_memberships").all())
        .toEqual([{ role: "owner" }]);
      expect(sqlite.query("SELECT role FROM dashboard_memberships").all())
        .toEqual([{ role: "admin" }]);
      expect(sqlite.query(`
        SELECT fingerprint, account_id FROM account_devices ORDER BY fingerprint
      `).all()).toEqual([{
        fingerprint: "browser-key",
        account_id: tenant.accountId,
      }]);

      for (const table of STRICT_SCOPE_TABLES) {
        expect(dashboardIds(sqlite, table)).toEqual(
          Array.from(
            { length: dashboardIds(sqlite, table).length },
            () => tenant.dashboardId,
          ),
        );
      }
      expect(dashboardIds(sqlite, "audit_log")).toEqual([tenant.dashboardId]);
      expect(sqlite.query(`
        SELECT key, dashboard_id FROM app_settings ORDER BY key
      `).all()).toEqual([
        { key: "push.vapid", dashboard_id: null },
        { key: "runtime-setting", dashboard_id: tenant.dashboardId },
      ]);

      sqlite.query(`
        INSERT INTO audit_log
          (ts, caller_fp, method, path, status, trace_id, dashboard_id)
        VALUES (2, NULL, 'GET', '/anonymous', 401, NULL, NULL)
      `).run();
      expect(ensureSelfHostedTenant(sqlite, {
        backfillLegacyScopes: false,
      })).toEqual(tenant);
      expect(dashboardIds(sqlite, "audit_log"))
        .toEqual([tenant.dashboardId, null]);
    } finally {
      sqlite.close();
    }
  });

  test("reuses a coherent active topology without renaming it", async () => {
    const sqlite = await migratedPre0024Database();
    try {
      seedExistingTopology(sqlite);
      insertAuthorizedKey(sqlite, "existing-browser", 3);

      const tenant = ensureSelfHostedTenant(sqlite, {
        backfillLegacyScopes: true,
      });

      expect(tenant).toEqual({
        accountId: "account-existing",
        organizationId: "organization-existing",
        dashboardId: "dashboard-existing",
      });
      expect(sqlite.query("SELECT slug, name FROM organizations").all())
        .toEqual([{
          slug: "kept-organization",
          name: "Kept organization",
        }]);
      expect(sqlite.query("SELECT slug, name FROM dashboards").all())
        .toEqual([{ slug: "kept-dashboard", name: "Kept dashboard" }]);
      expect(sqlite.query(`
        SELECT fingerprint, account_id FROM account_devices
      `).all()).toEqual([{
        fingerprint: "existing-browser",
        account_id: "account-existing",
      }]);
    } finally {
      sqlite.close();
    }
  });

  test("validation-only mode rejects every guarded null scope but accepts anonymous audit and global VAPID", async () => {
    const sqlite = await migratedPre0024Database();
    try {
      seedLegacyRuntimeRows(sqlite);
      const tenant = ensureSelfHostedTenant(sqlite, {
        backfillLegacyScopes: true,
      });
      sqlite.query(`
        INSERT INTO audit_log
          (ts, caller_fp, method, path, status, trace_id, dashboard_id)
        VALUES (2, NULL, 'GET', '/anonymous', 401, NULL, NULL)
      `).run();
      expect(ensureSelfHostedTenant(sqlite, {
        backfillLegacyScopes: false,
      })).toEqual(tenant);

      for (const table of STRICT_SCOPE_TABLES) {
        sqlite.query(`UPDATE ${table} SET dashboard_id = NULL`).run();
        const message = captureInvariantError(() => {
          ensureSelfHostedTenant(sqlite, { backfillLegacyScopes: false });
        });
        expect(message).toBe(
          `${INVARIANT_PREFIX}${table} contains missing or foreign dashboard scope`,
        );
        sqlite.query(`UPDATE ${table} SET dashboard_id = ?`)
          .run(tenant.dashboardId);
      }
      expect(ensureSelfHostedTenant(sqlite, {
        backfillLegacyScopes: false,
      })).toEqual(tenant);
      expect(dashboardIds(sqlite, "audit_log"))
        .toEqual([tenant.dashboardId, null]);
      expect(sqlite.query(`
        SELECT dashboard_id FROM app_settings WHERE key = 'push.vapid'
      `).all()).toEqual([{ dashboard_id: null }]);
    } finally {
      sqlite.close();
    }
  });

  test("rejects ambiguous, inactive, partial, foreign, and colliding topology with bounded errors", async () => {
    const cases: Array<{
      name: string;
      setup: (sqlite: Database) => void;
      reason: string;
    }> = [
      {
        name: "multiple accounts",
        setup: (sqlite) => {
          sqlite.query(`
            INSERT INTO accounts
              (id, email_normalized, password_hash, status, created_at_ms,
               password_changed_at_ms)
            VALUES
              ('account-1', 'one@example.com', NULL, 'active', 1, NULL),
              ('account-2', 'two@example.com', NULL, 'active', 1, NULL)
          `).run();
        },
        reason: "multiple accounts",
      },
      {
        name: "multiple organizations",
        setup: (sqlite) => {
          seedExistingTopology(sqlite);
          sqlite.query(`
            INSERT INTO organizations (id, slug, name, status, created_at_ms)
            VALUES ('organization-2', 'other', 'Other', 'active', 1)
          `).run();
        },
        reason: "multiple organizations",
      },
      {
        name: "multiple dashboards",
        setup: (sqlite) => {
          seedExistingTopology(sqlite);
          sqlite.query(`
            INSERT INTO dashboards
              (id, organization_id, slug, name, status, created_at_ms)
            VALUES (
              'dashboard-2', 'organization-existing', 'other', 'Other',
              'active', 1
            )
          `).run();
        },
        reason: "multiple dashboards",
      },
      {
        name: "inactive account",
        setup: (sqlite) => seedExistingTopology(sqlite, { account: "disabled" }),
        reason: "account is inactive",
      },
      {
        name: "inactive organization",
        setup: (sqlite) => seedExistingTopology(sqlite, {
          organization: "suspended",
        }),
        reason: "organization is inactive",
      },
      {
        name: "inactive dashboard",
        setup: (sqlite) => seedExistingTopology(sqlite, {
          dashboard: "suspended",
        }),
        reason: "dashboard is inactive",
      },
      {
        name: "partial membership",
        setup: (sqlite) => {
          seedExistingTopology(sqlite);
          sqlite.query("DELETE FROM dashboard_memberships").run();
        },
        reason: "identity topology is partial",
      },
      {
        name: "foreign runtime scope",
        setup: (sqlite) => {
          seedExistingTopology(sqlite);
          sqlite.exec("PRAGMA foreign_keys=OFF");
          sqlite.query(`
            INSERT INTO tasks
              (id, state, payload_json, enqueued_at_ms, claim_ttl_ms, dashboard_id)
            VALUES ('foreign-task', 'pending', '{}', 1, 1, 'foreign-dashboard')
          `).run();
          sqlite.exec("PRAGMA foreign_keys=ON");
        },
        reason: "tasks contains foreign dashboard scope",
      },
      {
        name: "worker and account-device collision",
        setup: (sqlite) => {
          seedExistingTopology(sqlite);
          insertAuthorizedKey(sqlite, "colliding-key", 4);
          insertWorker(sqlite, "colliding-key", "dashboard-existing");
          sqlite.query(`
            INSERT INTO account_devices
              (fingerprint, account_id, added_at_ms, last_seen_at_ms)
            VALUES ('colliding-key', 'account-existing', 1, 1)
          `).run();
        },
        reason: "authorized key is both a worker and account device",
      },
    ];

    for (const fixture of cases) {
      const sqlite = await migratedPre0024Database();
      try {
        fixture.setup(sqlite);
        const message = captureInvariantError(() => {
          ensureSelfHostedTenant(sqlite, { backfillLegacyScopes: true });
        });
        expect(message).toBe(`${INVARIANT_PREFIX}${fixture.reason}`);
        expect(message.length).toBeLessThan(160);
      } finally {
        sqlite.close();
      }
    }
  });

  test("rolls back tenant creation, key association, and scope writes together", async () => {
    const sqlite = await migratedPre0024Database();
    try {
      seedLegacyRuntimeRows(sqlite);
      sqlite.exec(`
        CREATE TRIGGER fail_event_scope
        BEFORE UPDATE OF dashboard_id ON events
        BEGIN
          SELECT RAISE(ABORT, 'fixture write failure');
        END
      `);

      const message = captureInvariantError(() => {
        ensureSelfHostedTenant(sqlite, { backfillLegacyScopes: true });
      });
      expect(message).toBe(`${INVARIANT_PREFIX}database operation failed`);
      expect(sqlite.query("SELECT COUNT(*) AS count FROM accounts").get())
        .toEqual({ count: 0 });
      expect(sqlite.query("SELECT COUNT(*) AS count FROM account_devices").get())
        .toEqual({ count: 0 });
      expect(dashboardIds(sqlite, "workers")).toEqual([null]);
      expect(dashboardIds(sqlite, "bootstrap_tokens")).toEqual([null]);
    } finally {
      sqlite.close();
    }
  });
});
