/**
 * Owns pre-0024 databases and scoped rows used by bootstrap-token migration tests.
 * The prefixed migration suites call these factories to isolate each in-memory case.
 * It depends on the real migration runner and legacy schema files under coord.
 */
import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { Database } from "bun:sqlite";
import { expect } from "bun:test";
import { runMigrations } from "../src/db/migrate.ts";

const migrationsDir = join(import.meta.dir, "../migrations");
const priorMigrations = readdirSync(migrationsDir)
  .filter((name) => name.endsWith(".sql") && name < "0024_auth_tenancy_stabilization.sql")
  .sort()
  .map((name) => ({
    name: name.slice(0, -4),
    sql: readFileSync(join(migrationsDir, name), "utf8"),
  }));
const migration0024 = {
  name: "0024_auth_tenancy_stabilization",
  sql: readFileSync(
    join(migrationsDir, "0024_auth_tenancy_stabilization.sql"),
    "utf8",
  ),
};

async function openPre0024(): Promise<Database> {
  const sqlite = new Database(":memory:");
  sqlite.exec("PRAGMA foreign_keys = ON");
  await runMigrations(sqlite, priorMigrations);
  return sqlite;
}

async function apply0024(sqlite: Database): Promise<void> {
  await runMigrations(sqlite, [...priorMigrations, migration0024]);
}

function seedTenant(
  sqlite: Database,
  suffix: string,
  status: "active" | "disabled" = "active",
): { accountId: string; organizationId: string; dashboardId: string } {
  const accountId = `account-${suffix}`;
  const organizationId = `organization-${suffix}`;
  const dashboardId = `dashboard-${suffix}`;
  sqlite.query(`
    INSERT INTO accounts
      (id, email_normalized, password_hash, status, created_at_ms,
       password_changed_at_ms)
    VALUES (?, ?, NULL, ?, 1, NULL)
  `).run(accountId, `${suffix}@example.test`, status);
  sqlite.query(`
    INSERT INTO organizations (id, slug, name, status, created_at_ms)
    VALUES (?, ?, ?, 'active', 1)
  `).run(organizationId, `org-${suffix}`, `Organization ${suffix}`);
  sqlite.query(`
    INSERT INTO organization_memberships
      (organization_id, account_id, role, created_at_ms)
    VALUES (?, ?, 'owner', 1)
  `).run(organizationId, accountId);
  sqlite.query(`
    INSERT INTO dashboards
      (id, organization_id, slug, name, status, created_at_ms)
    VALUES (?, ?, 'default', ?, 'active', 1)
  `).run(dashboardId, organizationId, `Dashboard ${suffix}`);
  sqlite.query(`
    INSERT INTO dashboard_memberships
      (dashboard_id, account_id, role, created_at_ms)
    VALUES (?, ?, 'admin', 1)
  `).run(dashboardId, accountId);
  return { accountId, organizationId, dashboardId };
}

function addDashboardMember(
  sqlite: Database,
  dashboardId: string,
  organizationId: string,
  suffix: string,
): string {
  const accountId = `account-${suffix}`;
  sqlite.query(`
    INSERT INTO accounts
      (id, email_normalized, password_hash, status, created_at_ms,
       password_changed_at_ms)
    VALUES (?, ?, NULL, 'active', 1, NULL)
  `).run(accountId, `${suffix}@example.test`);
  sqlite.query(`
    INSERT INTO organization_memberships
      (organization_id, account_id, role, created_at_ms)
    VALUES (?, ?, 'member', 1)
  `).run(organizationId, accountId);
  sqlite.query(`
    INSERT INTO dashboard_memberships
      (dashboard_id, account_id, role, created_at_ms)
    VALUES (?, ?, 'member', 1)
  `).run(dashboardId, accountId);
  return accountId;
}

function insertLegacyToken(
  sqlite: Database,
  row: {
    token: string | Uint8Array;
    dashboardId: string | null;
    kind?: string;
    label?: string;
    createdAtMs?: number;
    expiresAtMs: number;
    usedAtMs?: number | null;
    usedByFp?: string | null;
    mintedByFp?: string | null;
  },
): void {
  sqlite.query(`
    INSERT INTO bootstrap_tokens
      (token, dashboard_id, kind, label, created_at_ms, expires_at_ms,
       used_at_ms, used_by_fp, minted_by_fp)
    VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
  `).run(
    row.token,
    row.dashboardId,
    row.kind ?? "browser",
    row.label ?? "Legacy grant",
    row.createdAtMs ?? 10,
    row.expiresAtMs,
    row.usedAtMs ?? null,
    row.usedByFp ?? null,
    row.mintedByFp ?? null,
  );
}

function addAccountDevice(
  sqlite: Database,
  fingerprint: string,
  accountId: string,
): void {
  sqlite.query(`
    INSERT INTO authorized_keys (fingerprint, public_key, label, added_at)
    VALUES (?, ?, ?, 1)
  `).run(fingerprint, new Uint8Array(32), fingerprint);
  sqlite.query(`
    INSERT INTO account_devices
      (fingerprint, account_id, added_at_ms, last_seen_at_ms)
    VALUES (?, ?, 1, 1)
  `).run(fingerprint, accountId);
}

function expectSqlFailure(sqlite: Database, sql: string): void {
  expect(() => sqlite.exec(sql)).toThrow();
}

function insertWorker(
  sqlite: Database,
  fingerprint: string,
  dashboardId: string,
): void {
  sqlite.query(`
    INSERT INTO workers
      (fp, dashboard_id, label, os, git_sha, host_metrics_json,
       registered_at_ms, last_seen_ms, reachable_addr, keeper_stale)
    VALUES (?, ?, ?, 'linux', NULL, NULL, 1, 1, NULL, NULL)
  `).run(fingerprint, dashboardId, fingerprint);
}

function insertSession(
  sqlite: Database,
  id: string,
  workerFp: string,
  dashboardId: string,
): void {
  sqlite.query(`
    INSERT INTO sessions
      (id, dashboard_id, worker_fp, channel, kind, cwd, workspace_id,
       status, agent_json, created_at, closed_at, custom_title, git_branch,
       git_remote, pr_number, pr_state, pr_checks, pr_url, ports_json,
       spawn_cwd)
    VALUES (?, ?, ?, 1, 'shell', '/tmp', NULL, 'open', NULL, 1, NULL,
            NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, '/tmp')
  `).run(id, dashboardId, workerFp);
}

function insertWorkspace(
  sqlite: Database,
  id: string,
  workerFp: string,
  dashboardId: string,
): void {
  sqlite.query(`
    INSERT INTO workspaces
      (id, dashboard_id, worker_fp, name, folder_path, color, position,
       version, created_at_ms, updated_at_ms)
    VALUES (?, ?, ?, ?, '/tmp', NULL, 0, 0, 1, 1)
  `).run(id, dashboardId, workerFp, id);
}

export {
  addAccountDevice,
  addDashboardMember,
  apply0024,
  expectSqlFailure,
  insertLegacyToken,
  insertSession,
  insertWorker,
  insertWorkspace,
  openPre0024,
  seedTenant,
};
