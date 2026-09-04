// These fixtures own the legacy database states used by self-hosted tenant tests.
// The initialization and migration-prerequisite suites call them to share exact setup.
// They depend on the pre-tenancy migrations and in-memory SQLite only.
// Keeping the SQL here makes each test file small without hiding assertions.
import { Database } from "bun:sqlite";
import { readdir, readFile } from "node:fs/promises";
import { runMigrations } from "../src/db/migrate.ts";

const PRE_0024_MIGRATION_URL = new URL("../migrations/", import.meta.url);
export const INVARIANT_PREFIX = "self-hosted tenant invariant violation: ";
export const STRICT_SCOPE_TABLES = [
  "workers",
  "bootstrap_tokens",
  "events",
  "sessions",
  "workspaces",
  "workspace_sessions",
  "tasks",
  "mcp_relays",
  "push_subscriptions",
] as const;

let pre0024MigrationsPromise: Promise<Array<{ name: string; sql: string }>> | undefined;

async function pre0024Migrations(): Promise<Array<{ name: string; sql: string }>> {
  if (!pre0024MigrationsPromise) {
    pre0024MigrationsPromise = (async () => {
      const files = (await readdir(PRE_0024_MIGRATION_URL))
        .filter((file) => file.endsWith(".sql") && file < "0024_")
        .sort();
      return Promise.all(files.map(async (file) => ({
        name: file.replace(/\.sql$/, ""),
        sql: await readFile(new URL(file, PRE_0024_MIGRATION_URL), "utf8"),
      })));
    })();
  }
  return pre0024MigrationsPromise;
}

export async function migratedPre0024Database(): Promise<Database> {
  const sqlite = new Database(":memory:");
  sqlite.exec("PRAGMA foreign_keys=ON");
  await runMigrations(sqlite, await pre0024Migrations());
  return sqlite;
}

export function insertAuthorizedKey(
  sqlite: Database,
  fingerprint: string,
  fill: number,
): void {
  sqlite.query(`
    INSERT INTO authorized_keys (fingerprint, public_key, label, added_at)
    VALUES (?, ?, ?, 1)
  `).run(fingerprint, new Uint8Array(32).fill(fill), fingerprint);
}

export function insertWorker(
  sqlite: Database,
  fingerprint: string,
  dashboardId: string | null = null,
): void {
  sqlite.query(`
    INSERT INTO workers
      (fp, label, os, git_sha, host_metrics_json, registered_at_ms,
       last_seen_ms, reachable_addr, keeper_stale, dashboard_id)
    VALUES (?, ?, 'linux', NULL, NULL, 1, 1, ?, NULL, ?)
  `).run(fingerprint, fingerprint, `${fingerprint}.example`, dashboardId);
}

export function seedExistingTopology(
  sqlite: Database,
  status: {
    account?: "active" | "disabled";
    organization?: "active" | "suspended";
    dashboard?: "active" | "suspended";
  } = {},
): void {
  sqlite.query(`
    INSERT INTO accounts
      (id, email_normalized, password_hash, status, created_at_ms, password_changed_at_ms)
    VALUES ('account-existing', 'kept@example.com', NULL, ?, 1, NULL)
  `).run(status.account ?? "active");
  sqlite.query(`
    INSERT INTO organizations (id, slug, name, status, created_at_ms)
    VALUES ('organization-existing', 'kept-organization', 'Kept organization', ?, 1)
  `).run(status.organization ?? "active");
  sqlite.query(`
    INSERT INTO organization_memberships
      (organization_id, account_id, role, created_at_ms)
    VALUES ('organization-existing', 'account-existing', 'owner', 1)
  `).run();
  sqlite.query(`
    INSERT INTO dashboards
      (id, organization_id, slug, name, status, created_at_ms)
    VALUES (
      'dashboard-existing', 'organization-existing', 'kept-dashboard',
      'Kept dashboard', ?, 1
    )
  `).run(status.dashboard ?? "active");
  sqlite.query(`
    INSERT INTO dashboard_memberships
      (dashboard_id, account_id, role, created_at_ms)
    VALUES ('dashboard-existing', 'account-existing', 'admin', 1)
  `).run();
}

export function seedLegacyRuntimeRows(sqlite: Database): void {
  insertAuthorizedKey(sqlite, "browser-key", 1);
  insertAuthorizedKey(sqlite, "worker-key", 2);
  insertWorker(sqlite, "worker-key");
  sqlite.query(`
    INSERT INTO bootstrap_tokens
      (token, kind, label, created_at_ms, expires_at_ms, used_at_ms,
       used_by_fp, minted_by_fp)
    VALUES ('legacy-token', 'browser', 'legacy token', 1, 2, NULL, NULL, NULL)
  `).run();
  sqlite.query(`
    INSERT INTO events
      (kind, session_id, worker_fp, payload_json, ts, client_seq)
    VALUES ('snapshot', NULL, 'worker-key', '{}', 1, NULL)
  `).run();
  sqlite.query(`
    INSERT INTO sessions
      (id, worker_fp, channel, kind, cwd, status, created_at)
    VALUES ('session-1', 'worker-key', 1, 'shell', '/tmp', 'open', 1)
  `).run();
  sqlite.query(`
    INSERT INTO workspaces
      (id, worker_fp, name, position, version, created_at_ms, updated_at_ms)
    VALUES ('workspace-1', 'worker-key', 'Legacy', 0, 1, 1, 1)
  `).run();
  sqlite.query(`
    INSERT INTO workspace_sessions (workspace_id, session_id, added_at_ms)
    VALUES ('workspace-1', 'session-1', 1)
  `).run();
  sqlite.query(`
    INSERT INTO tasks
      (id, state, payload_json, enqueued_at_ms, claim_ttl_ms)
    VALUES ('task-1', 'pending', '{}', 1, 1)
  `).run();
  sqlite.query(`
    INSERT INTO mcp_relays (id, label, kind, config_json, created_at_ms)
    VALUES ('mcp-1', 'Legacy relay', 'stdio', '{}', 1)
  `).run();
  sqlite.query(`
    INSERT INTO audit_log (ts, caller_fp, method, path, status, trace_id)
    VALUES (1, 'browser-key', 'POST', '/legacy', 200, NULL)
  `).run();
  sqlite.query(`
    INSERT INTO app_settings (key, value, updated_at_ms)
    VALUES ('runtime-setting', 'value', 1)
  `).run();
  sqlite.query(`
    INSERT INTO app_settings (key, value, updated_at_ms)
    VALUES ('push.vapid', '{}', 1)
  `).run();
  sqlite.query(`
    INSERT INTO push_subscriptions
      (viewer_fp, endpoint, p256dh, auth, created_at_ms)
    VALUES ('browser-key', 'https://push.example/sub', 'p256dh', 'auth', 1)
  `).run();
}

export function dashboardIds(
  sqlite: Database,
  table: string,
): Array<string | null> {
  const statement = sqlite.prepare(
    `SELECT dashboard_id FROM ${table} ORDER BY rowid`,
  );
  try {
    return (statement.all() as Array<{ dashboard_id: string | null }>)
      .map((row) => row.dashboard_id);
  } finally {
    statement.finalize();
  }
}

export function captureInvariantError(run: () => void): string {
  try {
    run();
  } catch (error) {
    return error instanceof Error ? error.message : String(error);
  }
  throw new Error("expected self-hosted tenant initialization to fail");
}
