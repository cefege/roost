import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { Database } from "bun:sqlite";
import { describe, expect, test } from "bun:test";
import { runMigrations } from "../src/db/migrate.ts";

const migrationsDir = join(import.meta.dir, "../migrations");
const migrationFiles = readdirSync(migrationsDir)
  .filter((name) => name.endsWith(".sql"))
  .sort();
const migrations = migrationFiles.map((name) => ({
  name: name.slice(0, -4),
  sql: readFileSync(join(migrationsDir, name), "utf8"),
}));
const priorMigrations = migrations.filter(
  ({ name }) => name < "0026_drop_webhook_tokens_permission_rules",
);

function tableExists(sqlite: Database, name: string): boolean {
  return sqlite.query(`
    SELECT 1 AS present FROM sqlite_schema
    WHERE type = 'table' AND name = ?
  `).get(name) !== null;
}

function indexExists(sqlite: Database, name: string): boolean {
  return sqlite.query(`
    SELECT 1 AS present FROM sqlite_schema
    WHERE type = 'index' AND name = ?
  `).get(name) !== null;
}

describe("0026 retired settings migration", () => {
  test("discards retired rows while preserving MCP data and its index", async () => {
    const sqlite = new Database(":memory:");
    sqlite.exec("PRAGMA foreign_keys = ON");
    try {
      await runMigrations(sqlite, priorMigrations);
      sqlite.query(`
        INSERT INTO organizations (id, slug, name, status, created_at_ms)
        VALUES ('organization', 'personal', 'Personal', 'active', 1)
      `).run();
      sqlite.query(`
        INSERT INTO dashboards (
          id, organization_id, slug, name, status, created_at_ms
        ) VALUES ('dashboard', 'organization', 'default', 'Default', 'active', 1)
      `).run();
      sqlite.query(`
        INSERT INTO webhook_tokens (
          id, dashboard_id, label, hash, last4, scopes_json,
          created_at_ms, last_used_at_ms
        ) VALUES (
          'webhook', 'dashboard', 'Webhook', 'digest', '1234', '[]', 1, NULL
        )
      `).run();
      sqlite.query(`
        INSERT INTO permission_rules (
          id, dashboard_id, tool_pattern, folder_glob, decision, enabled,
          created_at_ms
        ) VALUES ('permission', 'dashboard', '*', '*', 'allow', 1, 1)
      `).run();
      sqlite.query(`
        INSERT INTO mcp_relays (
          id, dashboard_id, label, kind, config_json, created_at_ms
        ) VALUES ('mcp', 'dashboard', 'MCP', 'stdio', '{"command":"mcp"}', 1)
      `).run();

      await runMigrations(sqlite, migrations);

      expect(tableExists(sqlite, "webhook_tokens")).toBe(false);
      expect(tableExists(sqlite, "permission_rules")).toBe(false);
      expect(sqlite.query(`
        SELECT id, dashboard_id, label, kind, config_json, created_at_ms
        FROM mcp_relays
      `).all()).toEqual([{
        id: "mcp",
        dashboard_id: "dashboard",
        label: "MCP",
        kind: "stdio",
        config_json: '{"command":"mcp"}',
        created_at_ms: 1,
      }]);
      expect(indexExists(sqlite, "mcp_relays_dashboard_idx")).toBe(true);
    } finally {
      sqlite.close();
    }
  });

  test("fresh migration traversal never exposes the retired tables", async () => {
    const sqlite = new Database(":memory:");
    sqlite.exec("PRAGMA foreign_keys = ON");
    try {
      await runMigrations(sqlite, migrations);

      expect(tableExists(sqlite, "webhook_tokens")).toBe(false);
      expect(tableExists(sqlite, "permission_rules")).toBe(false);
      expect(tableExists(sqlite, "mcp_relays")).toBe(true);
      expect(indexExists(sqlite, "mcp_relays_dashboard_idx")).toBe(true);
    } finally {
      sqlite.close();
    }
  });
});
