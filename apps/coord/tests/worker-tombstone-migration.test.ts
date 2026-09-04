import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { Database } from "bun:sqlite";
import { describe, expect, test } from "bun:test";
import { runMigrations } from "../src/db/migrate.ts";

const migrationsDir = join(import.meta.dir, "../migrations");
const priorMigrations = readdirSync(migrationsDir)
  .filter((name) => name.endsWith(".sql") && name < "0025_worker_tombstones.sql")
  .sort()
  .map((name) => ({
    name: name.slice(0, -4),
    sql: readFileSync(join(migrationsDir, name), "utf8"),
  }));
const migration0025 = {
  name: "0025_worker_tombstones",
  sql: readFileSync(join(migrationsDir, "0025_worker_tombstones.sql"), "utf8"),
};

const DASHBOARD_A = "dashboard-a";
const DASHBOARD_B = "dashboard-b";
const ACTIVE_FP = "a".repeat(64);
const REVOKED_FP = "b".repeat(64);
const LEGACY_DANGLING_FP = "c".repeat(64);
const REVOKED_AT_MS = 1_700_000_000_123;

async function openPre0025(): Promise<Database> {
  const sqlite = new Database(":memory:");
  sqlite.exec("PRAGMA foreign_keys = ON");
  await runMigrations(sqlite, priorMigrations);
  return sqlite;
}

async function apply0025(sqlite: Database): Promise<void> {
  await runMigrations(sqlite, [...priorMigrations, migration0025]);
}

function seedDashboards(sqlite: Database): void {
  sqlite.query(`
    INSERT INTO accounts (
      id, email_normalized, password_hash, status, created_at_ms,
      password_changed_at_ms
    ) VALUES ('account', 'owner@example.test', NULL, 'active', 1, NULL)
  `).run();
  sqlite.query(`
    INSERT INTO organizations (id, slug, name, status, created_at_ms)
    VALUES ('organization', 'personal', 'Personal', 'active', 1)
  `).run();
  sqlite.query(`
    INSERT INTO organization_memberships (
      organization_id, account_id, role, created_at_ms
    ) VALUES ('organization', 'account', 'owner', 1)
  `).run();
  sqlite.query(`
    INSERT INTO dashboards (
      id, organization_id, slug, name, status, created_at_ms
    ) VALUES (?, 'organization', ?, ?, 'active', 1)
  `).run(DASHBOARD_A, "a", "Dashboard A");
  sqlite.query(`
    INSERT INTO dashboards (
      id, organization_id, slug, name, status, created_at_ms
    ) VALUES (?, 'organization', ?, ?, 'active', 1)
  `).run(DASHBOARD_B, "b", "Dashboard B");
}

function insertWorker(sqlite: Database, fingerprint: string): void {
  sqlite.query(`
    INSERT INTO authorized_keys (fingerprint, public_key, label, added_at)
    VALUES (?, ?, ?, 1)
  `).run(fingerprint, Buffer.alloc(32), fingerprint);
  sqlite.query(`
    INSERT INTO workers (
      fp, dashboard_id, label, os, registered_at_ms, last_seen_ms
    ) VALUES (?, ?, ?, 'linux', 1, 1)
  `).run(fingerprint, DASHBOARD_A, fingerprint);
}

function insertSession(
  sqlite: Database,
  id: string,
  workerFingerprint: string,
): void {
  sqlite.query(`
    INSERT INTO sessions (
      id, dashboard_id, worker_fp, channel, kind, cwd, workspace_id,
      status, created_at
    ) VALUES (?, ?, ?, 1, 'shell', '/tmp', NULL, 'open', 1)
  `).run(id, DASHBOARD_A, workerFingerprint);
}

function insertWorkspace(
  sqlite: Database,
  id: string,
  workerFingerprint: string,
): void {
  sqlite.query(`
    INSERT INTO workspaces (
      id, dashboard_id, worker_fp, name, folder_path, color, position,
      version, created_at_ms, updated_at_ms
    ) VALUES (?, ?, ?, ?, '/tmp', NULL, 0, 0, 1, 1)
  `).run(id, DASHBOARD_A, workerFingerprint, id);
}

function attachSession(sqlite: Database, workspaceId: string, sessionId: string): void {
  sqlite.query(`
    INSERT INTO workspace_sessions (
      workspace_id, dashboard_id, session_id, added_at_ms
    ) VALUES (?, ?, ?, 1)
  `).run(workspaceId, DASHBOARD_A, sessionId);
}

type ForeignKeyRow = {
  table: string;
  from: string;
  to: string;
  on_delete: string;
};

function foreignKeys(sqlite: Database, table: string): ForeignKeyRow[] {
  return sqlite.query(`PRAGMA foreign_key_list(${table})`).all() as ForeignKeyRow[];
}

const SCOPE_AND_RELATIONSHIP_TRIGGERS = [
  "workers_require_dashboard_insert",
  "workers_require_dashboard_update",
  "events_require_dashboard_insert",
  "events_require_dashboard_update",
  "sessions_require_dashboard_insert",
  "sessions_require_dashboard_update",
  "workspaces_require_dashboard_insert",
  "workspaces_require_dashboard_update",
  "workspace_sessions_require_dashboard_insert",
  "workspace_sessions_require_dashboard_update",
  "tasks_require_dashboard_insert",
  "tasks_require_dashboard_update",
  "mcp_relays_require_dashboard_insert",
  "mcp_relays_require_dashboard_update",
  "push_subscriptions_require_dashboard_insert",
  "push_subscriptions_require_dashboard_update",
  "sessions_require_scoped_worker_insert",
  "sessions_require_scoped_worker_update",
  "workspaces_require_scoped_worker_insert",
  "workspaces_require_scoped_worker_update",
  "workspace_sessions_require_scoped_parents_insert",
  "workspace_sessions_require_scoped_parents_update",
  "workers_preserve_child_dashboard_update",
] as const;

describe("0025 worker tombstone migration", () => {
  test("preserves worker history and repairs only already-dangling legacy rows", async () => {
    const sqlite = await openPre0025();
    try {
      seedDashboards(sqlite);
      insertWorker(sqlite, ACTIVE_FP);
      insertWorker(sqlite, REVOKED_FP);
      insertWorker(sqlite, LEGACY_DANGLING_FP);
      insertSession(sqlite, "active-session", ACTIVE_FP);
      insertSession(sqlite, "revoked-session", REVOKED_FP);
      insertWorkspace(sqlite, "retained-workspace", REVOKED_FP);
      attachSession(sqlite, "retained-workspace", "revoked-session");
      insertWorkspace(sqlite, "dangling-workspace", LEGACY_DANGLING_FP);
      attachSession(sqlite, "dangling-workspace", "active-session");
      insertSession(sqlite, "removed-session", ACTIVE_FP);
      insertWorkspace(sqlite, "link-repair-workspace", ACTIVE_FP);
      attachSession(sqlite, "link-repair-workspace", "removed-session");
      sqlite.query("DELETE FROM sessions WHERE id = 'removed-session'").run();
      expect(sqlite.query(`
        SELECT 1 AS present FROM workspace_sessions
        WHERE workspace_id = 'link-repair-workspace'
      `).get()).toEqual({ present: 1 });

      sqlite.query(`
        INSERT INTO authorized_key_revocations (
          fingerprint, revoked_at_ms, revoked_by_fp, reason
        ) VALUES (?, ?, 'administrator', 'legacy-revocation')
      `).run(REVOKED_FP, REVOKED_AT_MS);
      sqlite.query("DELETE FROM workers WHERE fp = ?").run(LEGACY_DANGLING_FP);
      sqlite.query("DELETE FROM authorized_keys WHERE fingerprint = ?")
        .run(LEGACY_DANGLING_FP);

      await apply0025(sqlite);

      expect(sqlite.query(`
        SELECT fp, deleted_at_ms FROM workers ORDER BY fp
      `).all()).toEqual([
        { fp: ACTIVE_FP, deleted_at_ms: null },
        { fp: REVOKED_FP, deleted_at_ms: REVOKED_AT_MS },
      ]);
      expect(sqlite.query(`
        SELECT fingerprint FROM authorized_keys ORDER BY fingerprint
      `).all()).toEqual([{ fingerprint: ACTIVE_FP }]);
      expect(sqlite.query(`
        SELECT id, worker_fp FROM sessions ORDER BY id
      `).all()).toEqual([
        { id: "active-session", worker_fp: ACTIVE_FP },
        { id: "revoked-session", worker_fp: REVOKED_FP },
      ]);
      expect(sqlite.query("SELECT id FROM workspaces ORDER BY id").all())
        .toEqual([
          { id: "link-repair-workspace" },
          { id: "retained-workspace" },
        ]);
      expect(sqlite.query(`
        SELECT workspace_id, session_id FROM workspace_sessions
      `).all()).toEqual([{
        workspace_id: "retained-workspace",
        session_id: "revoked-session",
      }]);
      expect(sqlite.query("PRAGMA foreign_key_check").all()).toEqual([]);

      expect(() => sqlite.query("DELETE FROM workers WHERE fp = ?").run(REVOKED_FP))
        .toThrow();
    } finally {
      sqlite.close();
    }
  });

  test("rebuilds restrictive foreign keys, indexes, and every 0024 write guard", async () => {
    const sqlite = await openPre0025();
    try {
      seedDashboards(sqlite);
      insertWorker(sqlite, ACTIVE_FP);
      insertSession(sqlite, "session", ACTIVE_FP);
      insertWorkspace(sqlite, "workspace", ACTIVE_FP);
      attachSession(sqlite, "workspace", "session");

      await apply0025(sqlite);

      expect(foreignKeys(sqlite, "sessions")).toContainEqual(expect.objectContaining({
        table: "workers",
        from: "worker_fp",
        to: "fp",
        on_delete: "NO ACTION",
      }));
      expect(foreignKeys(sqlite, "workspaces")).toContainEqual(expect.objectContaining({
        table: "workers",
        from: "worker_fp",
        to: "fp",
        on_delete: "NO ACTION",
      }));
      expect(foreignKeys(sqlite, "workspace_sessions")).toContainEqual(expect.objectContaining({
        table: "workspaces",
        from: "workspace_id",
        to: "id",
        on_delete: "CASCADE",
      }));
      expect(foreignKeys(sqlite, "workspace_sessions")).toContainEqual(expect.objectContaining({
        table: "sessions",
        from: "session_id",
        to: "id",
        on_delete: "CASCADE",
      }));
      expect(sqlite.query(`
        SELECT "notnull" AS is_not_null
        FROM pragma_table_info('sessions')
        WHERE name = 'worker_fp'
      `).get()).toEqual({ is_not_null: 1 });

      const activeIndex = sqlite.query(`
        SELECT sql FROM sqlite_master
        WHERE type = 'index' AND name = 'workers_dashboard_active_idx'
      `).get() as { sql: string };
      expect(activeIndex.sql).toContain("ON workers(dashboard_id, fp)");
      expect(activeIndex.sql).toContain("WHERE deleted_at_ms IS NULL");
      expect(sqlite.query(`
        SELECT name FROM sqlite_master
        WHERE type = 'index' AND name IN (
          'workspaces_dashboard_idx',
          'workspace_sessions_dashboard_session_idx'
        ) ORDER BY name
      `).all()).toEqual([
        { name: "workspace_sessions_dashboard_session_idx" },
        { name: "workspaces_dashboard_idx" },
      ]);

      const triggerNames = new Set((sqlite.query(`
        SELECT name FROM sqlite_master WHERE type = 'trigger'
      `).all() as Array<{ name: string }>).map(({ name }) => name));
      for (const trigger of SCOPE_AND_RELATIONSHIP_TRIGGERS) {
        expect(triggerNames.has(trigger)).toBe(true);
      }

      expect(() => sqlite.query(`
        INSERT INTO workspaces (
          id, dashboard_id, worker_fp, name, folder_path, position, version,
          created_at_ms, updated_at_ms
        ) VALUES ('null-scope', NULL, ?, 'Null', '/tmp', 0, 0, 1, 1)
      `).run(ACTIVE_FP)).toThrow();
      expect(() => sqlite.query(`
        INSERT INTO workspaces (
          id, dashboard_id, worker_fp, name, folder_path, position, version,
          created_at_ms, updated_at_ms
        ) VALUES ('cross-scope', ?, ?, 'Cross', '/tmp', 0, 0, 1, 1)
      `).run(DASHBOARD_B, ACTIVE_FP)).toThrow("workspace worker dashboard mismatch");
      expect(() => sqlite.query(`
        INSERT INTO workspace_sessions (
          workspace_id, dashboard_id, session_id, added_at_ms
        ) VALUES ('workspace', ?, 'session', 1)
      `).run(DASHBOARD_B)).toThrow("workspace session dashboard mismatch");

      sqlite.query("DELETE FROM sessions WHERE id = 'session'").run();
      expect(sqlite.query(`
        SELECT 1 AS present FROM workspace_sessions
        WHERE workspace_id = 'workspace'
      `).get()).toBeNull();
      expect(sqlite.query("PRAGMA foreign_key_check").all()).toEqual([]);
    } finally {
      sqlite.close();
    }
  });

  test("rolls back the migration when the JavaScript FK postcheck finds an injected violation", async () => {
    const sqlite = await openPre0025();
    try {
      seedDashboards(sqlite);
      insertWorker(sqlite, ACTIVE_FP);
      insertWorker(sqlite, REVOKED_FP);
      sqlite.query(`
        INSERT INTO authorized_key_revocations (
          fingerprint, revoked_at_ms, revoked_by_fp, reason
        ) VALUES (?, ?, 'administrator', 'legacy-revocation')
      `).run(REVOKED_FP, REVOKED_AT_MS);
      insertSession(sqlite, "orphaned-session", ACTIVE_FP);
      sqlite.exec("PRAGMA foreign_keys = OFF");
      sqlite.query("DELETE FROM workers WHERE fp = ?").run(ACTIVE_FP);
      sqlite.exec("PRAGMA foreign_keys = ON");
      expect(sqlite.query("PRAGMA foreign_key_check").all()).not.toEqual([]);

      await expect(apply0025(sqlite)).rejects.toThrow("0025 foreign key check failed");

      expect(sqlite.query(`
        SELECT 1 AS present FROM pragma_table_info('workers')
        WHERE name = 'deleted_at_ms'
      `).get()).toBeNull();
      expect(sqlite.query(`
        SELECT 1 AS present FROM sqlite_master
        WHERE type = 'index' AND name = 'workers_dashboard_active_idx'
      `).get()).toBeNull();
      expect(sqlite.query(`
        SELECT 1 AS present FROM _migrations
        WHERE name = '0025_worker_tombstones'
      `).get()).toBeNull();
      expect(sqlite.query(`
        SELECT 1 AS present FROM authorized_keys WHERE fingerprint = ?
      `).get(REVOKED_FP)).toEqual({ present: 1 });
      expect(foreignKeys(sqlite, "workspaces").some((row) => row.from === "worker_fp"))
        .toBe(false);
      expect(sqlite.query(`
        SELECT 1 AS present FROM sqlite_master
        WHERE type = 'trigger' AND name = 'workspaces_require_scoped_worker_insert'
      `).get()).toEqual({ present: 1 });
    } finally {
      sqlite.close();
    }
  });
});
