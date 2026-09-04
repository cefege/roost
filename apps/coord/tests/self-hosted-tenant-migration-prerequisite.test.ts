// These tests own the migration prerequisite behavior used by self-hosted tenancy.
// The coord migration runner calls the hook before each pending schema change.
// They depend on in-memory SQLite and the shared pre-tenancy database fixture.
// Keeping this suite separate isolates migration ordering from tenant initialization.
import { describe, expect, test } from "bun:test";
import { Database } from "bun:sqlite";
import { createHash } from "node:crypto";
import { mkdtempSync, readdirSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { makePreMigrationBackupHook } from "../src/backup.ts";
import { runMigrations } from "../src/db/migrate.ts";
import { ensureSelfHostedTenant } from "../src/self-hosted-tenant.ts";
import {
  dashboardIds,
  insertAuthorizedKey,
  migratedPre0024Database,
  STRICT_SCOPE_TABLES,
} from "./self-hosted-tenant-fixtures.ts";

const migrationsDir = join(import.meta.dir, "../migrations");
const allMigrations = readdirSync(migrationsDir)
  .filter((name) => name.endsWith(".sql"))
  .sort()
  .map((name) => ({
    name: name.slice(0, -4),
    sql: readFileSync(join(migrationsDir, name), "utf8"),
  }));
const managedMigrationNames = [
  "0020_saas_identity",
  "0021_dashboard_runtime_scope",
  "0022_owner_activation",
  "0023_federated_identity",
  "0024_auth_tenancy_stabilization",
  "0025_worker_tombstones",
  "0026_drop_webhook_tokens_permission_rules",
] as const;

function seedPre0020RuntimeRows(sqlite: Database): void {
  insertAuthorizedKey(sqlite, "browser-key", 1);
  insertAuthorizedKey(sqlite, "worker-key", 2);
  sqlite.exec(`
    INSERT INTO workers
      (fp, label, os, git_sha, host_metrics_json, registered_at_ms,
       last_seen_ms, reachable_addr, keeper_stale)
    VALUES (
      'worker-key', 'Legacy worker', 'linux', NULL, NULL, 1, 1,
      'worker.example', NULL
    );
    INSERT INTO bootstrap_tokens
      (token, kind, label, created_at_ms, expires_at_ms, used_at_ms,
       used_by_fp, minted_by_fp)
    VALUES (
      'legacy-token', 'browser', 'Legacy grant', 1, 2, NULL, NULL, NULL
    );
    INSERT INTO events
      (kind, session_id, worker_fp, payload_json, ts, client_seq)
    VALUES ('snapshot', NULL, 'worker-key', '{}', 1, NULL);
    INSERT INTO sessions
      (id, worker_fp, channel, kind, cwd, status, created_at)
    VALUES ('session-1', 'worker-key', 1, 'shell', '/tmp', 'open', 1);
    INSERT INTO workspaces
      (id, worker_fp, name, position, version, created_at_ms, updated_at_ms)
    VALUES ('workspace-1', 'worker-key', 'Legacy', 0, 1, 1, 1);
    INSERT INTO workspace_sessions (workspace_id, session_id, added_at_ms)
    VALUES ('workspace-1', 'session-1', 1);
    INSERT INTO tasks
      (id, state, payload_json, enqueued_at_ms, claim_ttl_ms)
    VALUES ('task-1', 'pending', '{}', 1, 1);
    INSERT INTO mcp_relays (id, label, kind, config_json, created_at_ms)
    VALUES ('mcp-1', 'Legacy relay', 'stdio', '{}', 1);
    INSERT INTO audit_log (ts, caller_fp, method, path, status, trace_id)
    VALUES (1, 'browser-key', 'POST', '/legacy', 200, NULL);
    INSERT INTO app_settings (key, value, updated_at_ms)
    VALUES ('runtime-setting', 'value', 1);
    INSERT INTO push_subscriptions
      (viewer_fp, endpoint, p256dh, auth, created_at_ms)
    VALUES ('browser-key', 'https://push.example/sub', 'p256dh', 'auth', 1);
    INSERT INTO webhook_tokens
      (id, label, hash, last4, scopes_json, created_at_ms)
    VALUES ('webhook-1', 'Legacy webhook', 'hash', 'hash', '[]', 1);
    INSERT INTO permission_rules
      (id, tool_pattern, folder_glob, decision, enabled, created_at_ms)
    VALUES ('rule-1', '*', '*', 'deny', 1, 1);
  `);
}

describe("migration prerequisite callback", () => {
  test("runs once immediately before each pending migration and outside its transaction", async () => {
    const sqlite = new Database(":memory:");
    const order: string[] = [];
    try {
      const migrations = [
        {
          name: "0001_first",
          sql: "CREATE TABLE first_table (id INTEGER PRIMARY KEY)",
        },
        {
          name: "0024_auth_tenancy_stabilization",
          sql: "CREATE TABLE guarded_table (id INTEGER PRIMARY KEY)",
        },
      ];
      await runMigrations(
        sqlite,
        migrations,
        async (pending) => {
          order.push(`backup:${pending.join(",")}`);
        },
        (name) => {
          order.push(`before:${name}`);
          if (name === "0024_auth_tenancy_stabilization") {
            expect(sqlite.query(`
              SELECT name FROM sqlite_schema
              WHERE type = 'table' AND name = 'first_table'
            `).get()).toEqual({ name: "first_table" });
            expect(sqlite.query(`
              SELECT name FROM _migrations WHERE name = '0001_first'
            `).get()).toEqual({ name: "0001_first" });
            expect(sqlite.inTransaction).toBe(false);
          }
        },
      );
      expect(order).toEqual([
        "backup:0001_first,0024_auth_tenancy_stabilization",
        "before:0001_first",
        "before:0024_auth_tenancy_stabilization",
      ]);

      order.length = 0;
      await runMigrations(
        sqlite,
        migrations,
        async () => {
          order.push("backup");
        },
        (name) => {
          order.push(`before:${name}`);
        },
      );
      expect(order).toEqual([]);
    } finally {
      sqlite.close();
    }
  });

  test("a managed migration run with no self-hosted hook leaves identity topology empty", async () => {
    const sqlite = await migratedPre0024Database();
    try {
      expect(sqlite.query("SELECT COUNT(*) AS count FROM accounts").get())
        .toEqual({ count: 0 });
      expect(sqlite.query("SELECT COUNT(*) AS count FROM organizations").get())
        .toEqual({ count: 0 });
      expect(sqlite.query("SELECT COUNT(*) AS count FROM dashboards").get())
        .toEqual({ count: 0 });
    } finally {
      sqlite.close();
    }
  });
  test("enables and rechecks foreign keys before applying each migration", async () => {
    const sqlite = new Database(":memory:");
    const callbacks: string[] = [];
    try {
      expect(sqlite.query("PRAGMA foreign_keys").get())
        .toEqual({ foreign_keys: 0 });
      await runMigrations(
        sqlite,
        [
          {
            name: "0002_child",
            sql: `CREATE TABLE child (
              id INTEGER PRIMARY KEY,
              parent_id INTEGER NOT NULL REFERENCES parent(id)
            )`,
          },
          {
            name: "0001_parent",
            sql: "CREATE TABLE parent (id INTEGER PRIMARY KEY)",
          },
        ],
        undefined,
        (name) => {
          callbacks.push(name);
          sqlite.exec("PRAGMA foreign_keys = OFF");
        },
      );

      expect(callbacks).toEqual(["0001_parent", "0002_child"]);
      expect(sqlite.query("PRAGMA foreign_keys").get())
        .toEqual({ foreign_keys: 1 });
      expect(() => sqlite.query(`
        INSERT INTO child (id, parent_id) VALUES (1, 404)
      `).run()).toThrow("FOREIGN KEY constraint failed");
    } finally {
      sqlite.close();
    }
  });

  test("rejects an FK-enabled caller transaction without touching its writes", async () => {
    const sqlite = new Database(":memory:");
    sqlite.exec(`
      PRAGMA foreign_keys = ON;
      CREATE TABLE parent (id INTEGER PRIMARY KEY);
      CREATE TABLE child (
        id INTEGER PRIMARY KEY,
        parent_id INTEGER NOT NULL REFERENCES parent(id)
      );
      BEGIN;
      INSERT INTO parent (id) VALUES (1);
      INSERT INTO child (id, parent_id) VALUES (1, 1)
    `);
    try {
      await expect(runMigrations(sqlite, [{
        name: "0001_forbidden",
        sql: "CREATE TABLE forbidden (id INTEGER PRIMARY KEY)",
      }])).rejects.toThrow("cannot use an open SQLite transaction");
      expect(sqlite.inTransaction).toBe(true);
      expect(sqlite.query("PRAGMA foreign_keys").get()).toEqual({ foreign_keys: 1 });
      expect(sqlite.query("SELECT id, parent_id FROM child").all())
        .toEqual([{ id: 1, parent_id: 1 }]);
      expect(sqlite.query(`
        SELECT name FROM sqlite_master
        WHERE name IN ('_migrations', 'forbidden')
      `).all()).toEqual([]);
    } finally {
      if (sqlite.inTransaction) sqlite.exec("ROLLBACK");
      sqlite.close();
    }
  });

  test.each(["beforeApply", "beforeMigration"] as const)(
    "leaves a transaction opened by the %s hook to its caller",
    async (hookKind) => {
      const sqlite = new Database(":memory:");
      sqlite.exec("CREATE TABLE caller_writes (value TEXT NOT NULL)");
      const openCallerTransaction = () => {
        sqlite.exec("BEGIN");
        sqlite.query("INSERT INTO caller_writes (value) VALUES (?)").run("preserved");
      };
      try {
        await expect(runMigrations(
          sqlite,
          [{
            name: "0001_forbidden",
            sql: "CREATE TABLE forbidden (id INTEGER PRIMARY KEY)",
          }],
          hookKind === "beforeApply" ? async () => openCallerTransaction() : undefined,
          hookKind === "beforeMigration" ? openCallerTransaction : undefined,
        )).rejects.toThrow("cannot use an open SQLite transaction");
        expect(sqlite.inTransaction).toBe(true);
        expect(sqlite.query("SELECT value FROM caller_writes").all())
          .toEqual([{ value: "preserved" }]);
        expect(sqlite.query("SELECT name FROM _migrations").all()).toEqual([]);
        expect(sqlite.query(`
          SELECT name FROM sqlite_master WHERE name = 'forbidden'
        `).get()).toBeNull();
      } finally {
        if (sqlite.inTransaction) sqlite.exec("ROLLBACK");
        sqlite.close();
      }
    },
  );

  test("rejects applied migration history with a gap", async () => {
    const sqlite = new Database(":memory:");
    sqlite.exec(`
      CREATE TABLE _migrations (
        name TEXT PRIMARY KEY,
        applied_at INTEGER NOT NULL
      );
      INSERT INTO _migrations (name, applied_at)
      VALUES ('0001_first', 1), ('0003_third', 3)
    `);
    try {
      await expect(runMigrations(sqlite, [
        { name: "0003_third", sql: "CREATE TABLE third_table (id INTEGER)" },
        { name: "0001_first", sql: "CREATE TABLE first_table (id INTEGER)" },
        { name: "0002_second", sql: "CREATE TABLE forbidden_second (id INTEGER)" },
      ])).rejects.toThrow("not an exact prefix");
      expect(sqlite.query(`
        SELECT name FROM sqlite_master WHERE name = 'forbidden_second'
      `).get()).toBeNull();
      expect(sqlite.query("SELECT name FROM _migrations ORDER BY name").all())
        .toEqual([{ name: "0001_first" }, { name: "0003_third" }]);
    } finally {
      sqlite.close();
    }
  });

  test("backs up and scopes pre-0020 rows before ordered 0020-0026 guards", async () => {
    const dir = mkdtempSync(join(tmpdir(), "roost-managed-migration-"));
    const dbPath = join(dir, "coord.db");
    const sqlite = new Database(dbPath, { create: true });
    try {
      const preManagedMigrations = allMigrations.filter(
        ({ name }) => name < managedMigrationNames[0],
      );
      const managedMigrations = allMigrations.filter(
        ({ name }) => name >= managedMigrationNames[0] && name < "0027_",
      );
      expect(managedMigrations.map(({ name }) => name))
        .toEqual([...managedMigrationNames]);

      await runMigrations(sqlite, preManagedMigrations);
      seedPre0020RuntimeRows(sqlite);

      const backup = makePreMigrationBackupHook(sqlite, dbPath);
      if (!backup) throw new Error("self-hosted backup hook is required");
      const order: string[] = [];
      let dashboardId: string | undefined;
      await runMigrations(
        sqlite,
        [...preManagedMigrations, ...managedMigrations].reverse(),
        async (pendingNames) => {
          order.push("backup");
          expect([...pendingNames]).toEqual([...managedMigrationNames]);
          await backup();
        },
        (name) => {
          order.push(name);
          if (name === "0024_auth_tenancy_stabilization") {
            dashboardId = ensureSelfHostedTenant(sqlite, {
              backfillLegacyScopes: true,
            }).dashboardId;
          }
        },
      );
      if (!dashboardId) throw new Error("self-hosted scope was not assigned");

      expect(order).toEqual(["backup", ...managedMigrationNames]);
      expect(readdirSync(join(dir, "backups"))
        .filter((name) => name.endsWith(".db.gz"))).toHaveLength(1);
      for (const table of STRICT_SCOPE_TABLES) {
        expect(dashboardIds(sqlite, table)).toEqual([dashboardId]);
      }
      expect(dashboardIds(sqlite, "audit_log")).toEqual([dashboardId]);
      expect(dashboardIds(sqlite, "app_settings")).toEqual([dashboardId]);
      expect(sqlite.query(`
        SELECT token_hash, account_id, dashboard_id FROM bootstrap_tokens
      `).get()).toEqual({
        token_hash: createHash("sha256").update("legacy-token").digest("hex"),
        account_id: sqlite.query<{ id: string }, []>(
          "SELECT id FROM accounts",
        ).get()!.id,
        dashboard_id: dashboardId,
      });
      expect(sqlite.query(`
        SELECT fingerprint FROM account_devices ORDER BY fingerprint
      `).all()).toEqual([{ fingerprint: "browser-key" }]);
      expect(sqlite.query(`
        SELECT name FROM _migrations
        WHERE name >= '0020_' AND name < '0027_'
        ORDER BY name
      `).all()).toEqual(
        managedMigrationNames.map((name) => ({ name })),
      );
      expect(sqlite.query(`
        SELECT name FROM sqlite_master
        WHERE name IN ('webhook_tokens', 'permission_rules')
      `).all()).toEqual([]);
      expect(sqlite.query("PRAGMA foreign_keys").get())
        .toEqual({ foreign_keys: 1 });
      expect(sqlite.query("PRAGMA foreign_key_check").all()).toEqual([]);
      expect(sqlite.query("PRAGMA integrity_check").all())
        .toEqual([{ integrity_check: "ok" }]);
    } finally {
      sqlite.close(true);
      rmSync(dir, { recursive: true, force: true });
    }
  });
});
