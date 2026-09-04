// Owns the database schema and pristine-state checks for managed owner bootstrap.
// The organizations command calls these checks around and within its bootstrap transaction.
// Keeping table scope here makes migration expectations and safety checks reviewable together.

import type { Database } from "bun:sqlite";

/** Runtime tables that managed bootstrap-owner requires to be pristine before
 * assigning them to the newly provisioned dashboard. */
export const BOOTSTRAP_OWNER_RUNTIME_SCOPE_TABLES = [
  "workers",
  "bootstrap_tokens",
  "events",
  "sessions",
  "workspaces",
  "workspace_sessions",
  "tasks",
  "mcp_relays",
  "audit_log",
  "app_settings",
  "push_subscriptions",
] as const;

export type BootstrapOwnerRuntimeScopeTable =
  typeof BOOTSTRAP_OWNER_RUNTIME_SCOPE_TABLES[number];

const REQUIRED_TABLES = [
  "accounts",
  "account_identities",
  "account_devices",
  "organizations",
  "organization_memberships",
  "dashboards",
  "dashboard_memberships",
  "owner_activation_tokens",
  "password_reset_tokens",
  "email_outbox",
  ...BOOTSTRAP_OWNER_RUNTIME_SCOPE_TABLES,
] as const;

/** Any row in these tables means this is either a completed bootstrap or a
 * partial SaaS state. Retrying through it could silently join unrelated data. */
export const MUST_BE_EMPTY = [
  "account_identities",
  "account_devices",
  "organizations",
  "organization_memberships",
  "dashboards",
  "dashboard_memberships",
  "owner_activation_tokens",
  "password_reset_tokens",
  "email_outbox",
] as const;

export function assertBootstrapSchema(sqlite: Database): void {
  for (const table of REQUIRED_TABLES) {
    const present = sqlite.query(
      "SELECT 1 AS present FROM sqlite_schema WHERE type = 'table' AND name = ?",
    ).get(table);
    if (!present) {
      throw new Error(
        `local coordinator database is missing required table ${table}; run the coordinator migrations before bootstrap-owner`,
      );
    }
  }

  for (const table of BOOTSTRAP_OWNER_RUNTIME_SCOPE_TABLES) {
    // These are explicit migration-contract queries. Do not put generated SQL
    // into bun:sqlite's bounded query cache.
    const statement = sqlite.prepare(`PRAGMA table_info(${table})`);
    try {
      const columns = statement.all() as Array<{ name: string }>;
      if (!columns.some((column) => column.name === "dashboard_id")) {
        throw new Error(
          `local coordinator database is missing ${table}.dashboard_id; run the dashboard scope migration before bootstrap-owner`,
        );
      }
    } finally {
      statement.finalize();
    }
  }
}

export function countBootstrapRows(sqlite: Database, table: string): number {
  const statement = sqlite.prepare(`SELECT COUNT(*) AS count FROM ${table}`);
  try {
    const row = statement.get();
    if (!row || typeof row !== "object" || !("count" in row) || typeof row.count !== "number") {
      throw new Error(`bootstrap-owner could not count ${table}`);
    }
    return row.count;
  } finally {
    statement.finalize();
  }
}

export function assertUnassignedOwnerRuntimeState(sqlite: Database): void {
  for (const table of BOOTSTRAP_OWNER_RUNTIME_SCOPE_TABLES) {
    const statement = sqlite.prepare(
      `SELECT 1 AS present FROM ${table} WHERE dashboard_id IS NOT NULL LIMIT 1`,
    );
    try {
      if (statement.get()) {
        throw new Error(`refusing bootstrap-owner: partial runtime scope in ${table}`);
      }
    } finally {
      statement.finalize();
    }
  }
}
