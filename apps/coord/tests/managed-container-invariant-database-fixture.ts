// Owns the in-memory SQLite states exercised by managed-container invariant suites.
// Prefixed discovered test modules call these seeds to share exact topology setup.
// Coord migrations and the invariant's runtime table inventory define its schema.
import { afterEach } from "bun:test";
import { Database } from "bun:sqlite";
import { runMigrations } from "../src/db/migrate.ts";
import { MANAGED_RUNTIME_SCOPE_TABLES } from "../src/managed-container-invariant.ts";

export const NOW = 1_800_000_000_000;
export const INSTANCE_ID = "11111111-1111-4111-8111-111111111111";
export const OTHER_INSTANCE_ID = "99999999-9999-4999-8999-999999999999";
export const ACCOUNT_ID = "22222222-2222-4222-8222-222222222222";
export const OTHER_ACCOUNT_ID = "33333333-3333-4333-8333-333333333333";
export const EMAIL = "owner@example.test";
export const OWNER_FP = "a".repeat(64);
export const WORKER_FP = "b".repeat(64);
const TENANT_ROUTE_KEY = "c".repeat(64);
export const MANAGED_CONFIG = {
  managedContainer: true,
  instanceId: INSTANCE_ID,
  tenantRouteKey: TENANT_ROUTE_KEY,
} as const;

export const databases: Database[] = [];
afterEach(() => {
  for (const sqlite of databases.splice(0)) sqlite.close();
});

export function emptyDatabase(): Database {
  const sqlite = new Database(":memory:");
  databases.push(sqlite);
  return sqlite;
}

export async function migratedDatabase(): Promise<Database> {
  const sqlite = emptyDatabase();
  sqlite.exec("PRAGMA foreign_keys=ON");
  await runMigrations(sqlite);
  return sqlite;
}

export function withoutForeignKeys(sqlite: Database, mutate: () => void): void {
  sqlite.exec("PRAGMA foreign_keys=OFF");
  try {
    mutate();
  } finally {
    sqlite.exec("PRAGMA foreign_keys=ON");
  }
}

export function seedActivation(sqlite: Database): void {
  sqlite.query(`
    INSERT INTO email_outbox (
      id, kind, recipient, encrypted_payload, idempotency_key, state,
      attempts, locked_until_ms, lease_token, next_attempt_ms,
      provider_message_id, sent_at_ms, failed_at_ms, last_error
    ) VALUES (?, 'owner_activation', ?, 'ciphertext', ?, 'pending',
              0, NULL, NULL, ?, NULL, NULL, NULL, NULL)
  `).run("activation-outbox", EMAIL, "activation-outbox", Number.MAX_SAFE_INTEGER);
  sqlite.query(`
    INSERT INTO owner_activation_tokens (
      coordinator_id, account_id, email_normalized, token_hash, outbox_id,
      delivery, created_at_ms, expires_at_ms, accepted_at_ms, revoked_at_ms
    ) VALUES (
      ?, ?, ?, '${"a".repeat(64)}', 'activation-outbox', 'coordinator-email',
      ?, ?, NULL, NULL
    )
  `).run(INSTANCE_ID, ACCOUNT_ID, EMAIL, NOW - 1_000, NOW + 60_000);
}

export function seedActiveOwner(sqlite: Database): void {
  seedActivation(sqlite);
  sqlite.query(
    "UPDATE owner_activation_tokens SET accepted_at_ms = ?",
  ).run(NOW);
  sqlite.query(`
    INSERT INTO accounts (
      id, email_normalized, password_hash, status, created_at_ms,
      password_changed_at_ms
    ) VALUES (?, ?, 'password-hash', 'active', ?, ?)
  `).run(ACCOUNT_ID, EMAIL, NOW, NOW);
  sqlite.query(`
    INSERT INTO account_identities (
      account_id, issuer, subject, email_normalized, linked_at_ms,
      last_authenticated_at_ms, revoked_at_ms
    ) VALUES (?, 'native', ?, ?, ?, NULL, NULL)
  `).run(ACCOUNT_ID, ACCOUNT_ID, EMAIL, NOW);
  sqlite.query(`
    INSERT INTO organizations (id, slug, name, status, created_at_ms)
    VALUES (?, 'personal', ?, 'active', ?)
  `).run(ACCOUNT_ID, EMAIL, NOW);
  sqlite.query(`
    INSERT INTO organization_memberships (
      organization_id, account_id, role, created_at_ms
    ) VALUES (?, ?, 'owner', ?)
  `).run(ACCOUNT_ID, ACCOUNT_ID, NOW);
  sqlite.query(`
    INSERT INTO dashboards (
      id, organization_id, slug, name, status, created_at_ms
    ) VALUES (?, ?, 'default', 'Personal', 'active', ?)
  `).run(INSTANCE_ID, ACCOUNT_ID, NOW);
  sqlite.query(`
    INSERT INTO dashboard_memberships (
      dashboard_id, account_id, role, created_at_ms
    ) VALUES (?, ?, 'admin', ?)
  `).run(INSTANCE_ID, ACCOUNT_ID, NOW);
  sqlite.query(`
    INSERT INTO authorized_keys (fingerprint, public_key, label, added_at)
    VALUES (?, ?, 'Owner browser', ?)
  `).run(OWNER_FP, Buffer.alloc(32, 1), NOW);
  sqlite.query(`
    INSERT INTO account_devices (
      fingerprint, account_id, added_at_ms, last_seen_at_ms
    ) VALUES (?, ?, ?, ?)
  `).run(OWNER_FP, ACCOUNT_ID, NOW, NOW);
}

export function ensureWorker(sqlite: Database): void {
  sqlite.query(`
    INSERT OR IGNORE INTO authorized_keys (fingerprint, public_key, label, added_at)
    VALUES (?, ?, 'Worker', ?)
  `).run(WORKER_FP, Buffer.alloc(32, 2), NOW);
  sqlite.query(`
    INSERT OR IGNORE INTO workers (
      fp, dashboard_id, label, os, git_sha, host_metrics_json,
      registered_at_ms, last_seen_ms, reachable_addr, keeper_stale
    ) VALUES (?, ?, 'Worker', 'linux', NULL, NULL, ?, ?, NULL, NULL)
  `).run(WORKER_FP, INSTANCE_ID, NOW, NOW);
}

export function insertRuntimeRow(
  sqlite: Database,
  table: typeof MANAGED_RUNTIME_SCOPE_TABLES[number],
  dashboardId: string | null,
  bypassForeignKeys = true,
): void {
  const insert = (): void => {
    switch (table) {
      case "workers":
        sqlite.query(`
          INSERT OR IGNORE INTO authorized_keys (fingerprint, public_key, label, added_at)
          VALUES (?, ?, 'Worker', ?)
        `).run(WORKER_FP, Buffer.alloc(32, 2), NOW);
        sqlite.query(`
          INSERT INTO workers (
            fp, dashboard_id, label, os, git_sha, host_metrics_json,
            registered_at_ms, last_seen_ms, reachable_addr, keeper_stale
          ) VALUES (?, ?, 'Worker', 'linux', NULL, NULL, ?, ?, NULL, NULL)
        `).run(WORKER_FP, dashboardId, NOW, NOW);
        break;
      case "bootstrap_tokens":
        sqlite.query(`
          INSERT INTO bootstrap_tokens (
            token_hash, account_id, dashboard_id, kind, label, created_at_ms,
            expires_at_ms, used_at_ms, used_by_fp, minted_by_fp
          ) VALUES (?, ?, ?, 'worker', 'Worker', ?, ?, NULL, NULL, ?)
        `).run("d".repeat(64), ACCOUNT_ID, dashboardId, NOW, NOW + 60_000, OWNER_FP);
        break;
      case "events":
        sqlite.query(`
          INSERT INTO events (
            dashboard_id, kind, session_id, worker_fp, payload_json, ts, client_seq
          ) VALUES (?, 'snapshot', NULL, NULL, '{}', ?, NULL)
        `).run(dashboardId, NOW);
        break;
      case "sessions":
        ensureWorker(sqlite);
        sqlite.query(`
          INSERT INTO sessions (
            id, dashboard_id, worker_fp, channel, kind, cwd, workspace_id,
            status, agent_json, created_at, closed_at, custom_title,
            git_branch, git_remote, pr_number, pr_state, pr_checks, pr_url,
            ports_json, spawn_cwd
          ) VALUES (
            'runtime-session', ?, ?, 1, 'shell', '/tmp', NULL,
            'open', NULL, ?, NULL, NULL,
            NULL, NULL, NULL, NULL, NULL, NULL,
            NULL, '/tmp'
          )
        `).run(dashboardId, WORKER_FP, NOW);
        break;
      case "workspaces":
        ensureWorker(sqlite);
        sqlite.query(`
          INSERT INTO workspaces (
            id, dashboard_id, worker_fp, name, folder_path, color, position,
            version, created_at_ms, updated_at_ms
          ) VALUES ('runtime-workspace', ?, ?, 'Workspace', '/tmp', NULL, 0, 0, ?, ?)
        `).run(dashboardId, WORKER_FP, NOW, NOW);
        break;
      case "workspace_sessions":
        ensureWorker(sqlite);
        sqlite.query(`
          INSERT INTO workspaces (
            id, dashboard_id, worker_fp, name, folder_path, color, position,
            version, created_at_ms, updated_at_ms
          ) VALUES ('join-workspace', ?, ?, 'Workspace', '/tmp', NULL, 0, 0, ?, ?)
        `).run(INSTANCE_ID, WORKER_FP, NOW, NOW);
        sqlite.query(`
          INSERT INTO sessions (
            id, dashboard_id, worker_fp, channel, kind, cwd, workspace_id,
            status, agent_json, created_at, closed_at, custom_title,
            git_branch, git_remote, pr_number, pr_state, pr_checks, pr_url,
            ports_json, spawn_cwd
          ) VALUES (
            'join-session', ?, ?, 2, 'shell', '/tmp', NULL,
            'open', NULL, ?, NULL, NULL,
            NULL, NULL, NULL, NULL, NULL, NULL,
            NULL, '/tmp'
          )
        `).run(INSTANCE_ID, WORKER_FP, NOW);
        sqlite.query(`
          INSERT INTO workspace_sessions (
            workspace_id, dashboard_id, session_id, added_at_ms
          ) VALUES ('join-workspace', ?, 'join-session', ?)
        `).run(dashboardId, NOW);
        break;
      case "tasks":
        sqlite.query(`
          INSERT INTO tasks (
            id, dashboard_id, state, payload_json, enqueued_at_ms,
            claimed_at_ms, claimed_by, finished_at_ms, result_json,
            completion_check, completion_check_last_attempt_ms, claim_ttl_ms
          ) VALUES ('runtime-task', ?, 'pending', '{}', ?, NULL, NULL, NULL,
                    NULL, NULL, NULL, 60000)
        `).run(dashboardId, NOW);
        break;
      case "mcp_relays":
        sqlite.query(`
          INSERT INTO mcp_relays (
            id, dashboard_id, label, kind, config_json, created_at_ms
          ) VALUES ('runtime-relay', ?, 'Relay', 'stdio', '{}', ?)
        `).run(dashboardId, NOW);
        break;
      case "audit_log":
        sqlite.query(`
          INSERT INTO audit_log (
            dashboard_id, ts, caller_fp, method, path, status, trace_id
          ) VALUES (?, ?, NULL, 'POST', '/test', 200, NULL)
        `).run(dashboardId, NOW);
        break;
      case "app_settings":
        sqlite.query(`
          INSERT INTO app_settings (dashboard_id, key, value, updated_at_ms)
          VALUES (?, 'runtime-setting', 'value', ?)
        `).run(dashboardId, NOW);
        break;
      case "push_subscriptions":
        sqlite.query(`
          INSERT INTO push_subscriptions (
            dashboard_id, viewer_fp, endpoint, p256dh, auth, created_at_ms
          ) VALUES (?, ?, 'https://push.example/runtime', 'p256dh', 'auth', ?)
        `).run(dashboardId, OWNER_FP, NOW);
        break;
    }
  };
  if (bypassForeignKeys) withoutForeignKeys(sqlite, insert);
  else insert();
}
