/**
 * Owns migration coverage for mandatory dashboard scopes and cross-scope rejection.
 * Bun discovers this suite separately from bootstrap-token row conversion cases.
 * It depends on the shared pre-0024 database and tenant fixture builders.
 */
import { describe, expect, test } from "bun:test";
import {
  apply0024,
  expectSqlFailure,
  insertSession,
  insertWorker,
  insertWorkspace,
  openPre0024,
  seedTenant,
} from "./bootstrap-token-migration-fixtures.ts";

describe("0024 auth and tenancy stabilization migration", () => {
  test("guards every mandatory scope and all same-dashboard relationships", async () => {
    const sqlite = await openPre0024();
    const first = seedTenant(sqlite, "guard-a");
    const second = seedTenant(sqlite, "guard-b");
    try {
      await apply0024(sqlite);

      const scopeTables = [
        "workers",
        "events",
        "sessions",
        "workspaces",
        "workspace_sessions",
        "tasks",
        "mcp_relays",
        "push_subscriptions",
      ];
      const triggerNames = new Set((sqlite.query(`
        SELECT name FROM sqlite_master WHERE type = 'trigger'
      `).all() as Array<{ name: string }>).map(({ name }) => name));
      for (const table of scopeTables) {
        expect(triggerNames.has(`${table}_require_dashboard_insert`)).toBe(true);
        expect(triggerNames.has(`${table}_require_dashboard_update`)).toBe(true);
      }

      const nullScopeInserts = [
        `INSERT INTO workers
          (fp, dashboard_id, label, os, git_sha, host_metrics_json,
           registered_at_ms, last_seen_ms, reachable_addr, keeper_stale)
         VALUES ('null-worker', NULL, 'null', 'linux', NULL, NULL, 1, 1, NULL, NULL)`,
        `INSERT INTO events
          (dashboard_id, kind, session_id, worker_fp, payload_json, ts, client_seq)
         VALUES (NULL, 'snapshot', NULL, NULL, '{}', 1, NULL)`,
        `INSERT INTO sessions
          (id, dashboard_id, worker_fp, channel, kind, cwd, workspace_id,
           status, agent_json, created_at, closed_at, custom_title, git_branch,
           git_remote, pr_number, pr_state, pr_checks, pr_url, ports_json,
           spawn_cwd)
         VALUES ('null-session', NULL, 'missing', 1, 'shell', '/tmp', NULL,
                 'open', NULL, 1, NULL, NULL, NULL, NULL, NULL, NULL, NULL,
                 NULL, NULL, '/tmp')`,
        `INSERT INTO workspaces
          (id, dashboard_id, worker_fp, name, folder_path, color, position,
           version, created_at_ms, updated_at_ms)
         VALUES ('null-workspace', NULL, 'missing', 'null', '/tmp', NULL,
                 0, 0, 1, 1)`,
        `INSERT INTO workspace_sessions
          (workspace_id, dashboard_id, session_id, added_at_ms)
         VALUES ('missing', NULL, 'missing', 1)`,
        `INSERT INTO tasks
          (id, dashboard_id, state, payload_json, enqueued_at_ms, claimed_at_ms,
           claimed_by, finished_at_ms, result_json, completion_check,
           completion_check_last_attempt_ms, claim_ttl_ms)
         VALUES ('null-task', NULL, 'pending', '{}', 1, NULL, NULL, NULL, NULL,
                 NULL, NULL, 1000)`,
        `INSERT INTO mcp_relays
          (id, dashboard_id, label, kind, config_json, created_at_ms)
         VALUES ('null-mcp', NULL, 'null', 'stdio', '{}', 1)`,
        `INSERT INTO push_subscriptions
          (dashboard_id, viewer_fp, endpoint, p256dh, auth, created_at_ms)
         VALUES (NULL, 'viewer', 'https://push.test/null', 'key', 'auth', 1)`,
      ];
      for (const sql of nullScopeInserts) expectSqlFailure(sqlite, sql);
      expectSqlFailure(sqlite, `
        INSERT INTO bootstrap_tokens
          (token_hash, account_id, dashboard_id, kind, label, created_at_ms,
           expires_at_ms, used_at_ms, used_by_fp, minted_by_fp)
        VALUES ('${"1".repeat(64)}', '${first.accountId}', NULL, 'browser',
                'null', 1, 2, NULL, NULL, NULL)
      `);

      insertWorker(sqlite, "worker-a", first.dashboardId);
      insertWorker(sqlite, "worker-b", second.dashboardId);
      expectSqlFailure(sqlite, `
        INSERT INTO sessions
          (id, dashboard_id, worker_fp, channel, kind, cwd, workspace_id,
           status, agent_json, created_at, closed_at, custom_title, git_branch,
           git_remote, pr_number, pr_state, pr_checks, pr_url, ports_json,
           spawn_cwd)
        VALUES ('cross-session', '${second.dashboardId}', 'worker-a', 1,
                'shell', '/tmp', NULL, 'open', NULL, 1, NULL, NULL, NULL,
                NULL, NULL, NULL, NULL, NULL, NULL, '/tmp')
      `);
      expectSqlFailure(sqlite, `
        INSERT INTO workspaces
          (id, dashboard_id, worker_fp, name, folder_path, color, position,
           version, created_at_ms, updated_at_ms)
        VALUES ('cross-workspace', '${second.dashboardId}', 'worker-a',
                'cross', '/tmp', NULL, 0, 0, 1, 1)
      `);

      insertSession(sqlite, "session-a", "worker-a", first.dashboardId);
      insertWorkspace(sqlite, "workspace-a", "worker-a", first.dashboardId);
      expectSqlFailure(sqlite, `
        INSERT INTO workspace_sessions
          (workspace_id, dashboard_id, session_id, added_at_ms)
        VALUES ('workspace-a', '${second.dashboardId}', 'session-a', 1)
      `);
      sqlite.query(`
        INSERT INTO workspace_sessions
          (workspace_id, dashboard_id, session_id, added_at_ms)
        VALUES ('workspace-a', ?, 'session-a', 1)
      `).run(first.dashboardId);
      expectSqlFailure(sqlite, `
        UPDATE workers SET dashboard_id = '${second.dashboardId}'
        WHERE fp = 'worker-a'
      `);
      expectSqlFailure(sqlite, `
        UPDATE sessions SET dashboard_id = '${second.dashboardId}',
                            worker_fp = 'worker-b'
        WHERE id = 'session-a'
      `);
      expectSqlFailure(sqlite, `
        UPDATE workspaces SET dashboard_id = '${second.dashboardId}',
                              worker_fp = 'worker-b'
        WHERE id = 'workspace-a'
      `);
      sqlite.query(`
        INSERT INTO events
          (dashboard_id, kind, session_id, worker_fp, payload_json, ts,
           client_seq)
        VALUES (?, 'guard-update', NULL, NULL, '{}', 1, NULL)
      `).run(first.dashboardId);
      expectSqlFailure(sqlite, `
        UPDATE events SET dashboard_id = NULL WHERE kind = 'guard-update'
      `);

      sqlite.exec(`
        INSERT INTO audit_log
          (dashboard_id, ts, caller_fp, method, path, status, trace_id)
        VALUES (NULL, 1, NULL, 'GET', '/anonymous', 401, NULL)
      `);
      sqlite.exec(`
        INSERT INTO app_settings (dashboard_id, key, value, updated_at_ms)
        VALUES (NULL, 'push.vapid', '{}', 1)
      `);
    } finally {
      sqlite.close(true);
    }
  });
});
