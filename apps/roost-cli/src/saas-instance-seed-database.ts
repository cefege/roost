/**
 * Centralizes database guards and transactions used by managed-instance seeding.
 * Owner activation and Google bootstrap flows share these checks before inserting identity rows.
 * Exact empty-topology checks make retries idempotent without accepting partially seeded state.
 */

import type { Database } from "bun:sqlite";
import { normalizeAccountEmail } from "@roost/shared/native-credentials";
import {
  canonicalSeedUuid,
  saasInstanceUsageError,
} from "./saas-instance-command.ts";
import type { SeedOwnerActivationInput } from "./saas-instance-types.ts";

export const OWNER_ACTIVATION_KIND = "owner_activation";
export const OWNER_ACTIVATION_TTL_MS = 7 * 24 * 60 * 60 * 1_000;
export const HELD_OUTBOX_TIME_MS = Number.MAX_SAFE_INTEGER;

export const EXACT_SEED_EMPTY_TABLES = [
  "owner_activation_tokens",
  "account_devices",
  "authorized_keys",
  "authorized_key_revocations",
  "email_outbox",
  "password_reset_tokens",
  "federated_assertion_redemptions",
  "workers",
  "bootstrap_tokens",
  "events",
  "sessions",
  "workspaces",
  "workspace_sessions",
  "tasks",
  "mcp_relays",
  "push_subscriptions",
  "pair_requests",
] as const;

export function checkedOwnerActivationTime(now: () => number): number {
  const value = now();
  if (!Number.isSafeInteger(value) || value < 0 || value > HELD_OUTBOX_TIME_MS - OWNER_ACTIVATION_TTL_MS) {
    throw new Error("owner activation clock returned an invalid millisecond timestamp");
  }
  return value;
}

export function countInstanceRows(sqlite: Database, table: string): number {
  const statement = sqlite.prepare(`SELECT COUNT(*) AS count FROM ${table}`);
  try {
    const row = statement.get() as { count?: unknown } | null;
    if (!row || typeof row.count !== "number" || !Number.isSafeInteger(row.count) || row.count < 0) {
      throw new Error("owner activation database returned an invalid row count");
    }
    return row.count;
  } finally {
    statement.finalize();
  }
}

export function withInstanceTransaction<T>(
  sqlite: Database,
  begin: "BEGIN" | "BEGIN IMMEDIATE",
  operation: () => T,
): T {
  sqlite.exec(begin);
  let open = true;
  try {
    const result = operation();
    sqlite.exec("COMMIT");
    open = false;
    return result;
  } catch (error) {
    if (open) {
      try {
        sqlite.exec("ROLLBACK");
      } catch {
        // Preserve the command failure that caused the rollback.
      }
    }
    throw error;
  }
}

export function normalizedOwnerActivationInput(
  input: SeedOwnerActivationInput,
): SeedOwnerActivationInput {
  const accountId = canonicalSeedUuid(input.accountId, "--account-id");
  const coordinatorId = canonicalSeedUuid(input.coordinatorId, "--coordinator-id");
  const email = normalizeAccountEmail(input.email);
  if (!email) throw saasInstanceUsageError("--email must be a valid account email");
  return { accountId, coordinatorId, email };
}

export function requireEmptySeedTables(sqlite: Database, tables: readonly string[]): void {
  for (const table of tables) {
    if (countInstanceRows(sqlite, table) !== 0) {
      throw new Error("refusing instance seed because the database topology is not empty");
    }
  }
}
