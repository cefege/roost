// Custom migration runner. Reads *.sql files from apps/coord/migrations/
// in lex order, and requires the applied history to be an exact prefix.
// Does NOT use Kysely's Migrator (subpath import issue in bun bundler mode);
// uses raw bun:sqlite Database instead. CRITICAL: throws on any failure.
// Enables foreign keys before schema writes and validates the final transaction.

import { readdir, readFile } from "node:fs/promises";
import { createHash } from "node:crypto";
import { join, resolve, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import type { Database } from "bun:sqlite";
import { log } from "@roost/shared/log";
import {
  assertNoOpenTransaction,
  enableAndVerifyForeignKeys,
  validateForeignKeys,
  validateIntegrity,
} from "./migration-validation.ts";

const MIGRATIONS_DIR = resolve(
  dirname(fileURLToPath(import.meta.url)),
  "../../migrations",
);

const AUTH_TENANCY_MIGRATION = "0024_auth_tenancy_stabilization";
const WORKER_TOMBSTONES_MIGRATION = "0025_worker_tombstones";
const RELOCATION_SENTINEL_PREFIX = "roost_move_";
const ROOST_SHA256_HEX_TEMP_TABLE = "_roost_sha256_hex_0024";
const AUTH_TENANCY_CONTEXT_TEMP_TABLE = "_roost_0024_context";
const MAX_MIGRATION_TOKEN_BYTES = 4_096;
const MANDATORY_DASHBOARD_SCOPE_TABLES = [
  "workers",
  "events",
  "sessions",
  "workspaces",
  "workspace_sessions",
  "tasks",
  "mcp_relays",
  "push_subscriptions",
] as const;

interface AuthTenancyPreflight {
  ordinaryCount: number;
  eligibleRelocationCount: number;
}

/**
 * Bun 1.3.14 cannot register a JavaScript SQLite UDF and Database.exec can
 * suppress constraint failures from compound SQL. Perform the authority and
 * relationship preflight in JavaScript, and stage the migration-only
 * roost_sha256_hex equivalent plus its frozen eligibility instant in
 * connection-local TEMP tables inside the migration transaction.
 */
function stageAuthTenancyPreflight(sqlite: Database): AuthTenancyPreflight {
  sqlite.exec(`
    CREATE TEMP TABLE ${ROOST_SHA256_HEX_TEMP_TABLE} (
      plaintext TEXT PRIMARY KEY NOT NULL,
      token_hash TEXT NOT NULL UNIQUE CHECK (
        length(token_hash) = 64
        AND token_hash = lower(token_hash)
        AND token_hash NOT GLOB '*[^0-9a-f]*'
      )
    );
    CREATE TEMP TABLE ${AUTH_TENANCY_CONTEXT_TEMP_TABLE} (
      now_ms INTEGER NOT NULL
    )
  `);
  const insertContext = sqlite.prepare(`
    INSERT INTO ${AUTH_TENANCY_CONTEXT_TEMP_TABLE} (now_ms) VALUES (?)
  `);
  try {
    insertContext.run(Date.now());
  } finally {
    insertContext.finalize();
  }

  const authority = sqlite.prepare(`
    SELECT count(*) AS invalid_count
    FROM bootstrap_tokens AS source
    WHERE NOT (
      typeof(source.token) = 'text'
      AND substr(source.token, 1, length('roost_move_')) = 'roost_move_'
    )
      AND (
        SELECT count(*)
        FROM dashboard_memberships AS membership
        WHERE membership.dashboard_id = source.dashboard_id
      ) <> 1
  `);
  try {
    const row = authority.get() as { invalid_count: number };
    if (row.invalid_count !== 0) {
      throw new Error("0024 ordinary bootstrap token authority is not exact");
    }
  } finally {
    authority.finalize();
  }

  for (const table of MANDATORY_DASHBOARD_SCOPE_TABLES) {
    const scope = sqlite.prepare(`
      SELECT count(*) AS invalid_count FROM ${table}
      WHERE dashboard_id IS NULL
    `);
    try {
      const row = scope.get() as { invalid_count: number };
      if (row.invalid_count !== 0) {
        throw new Error(`0024 mandatory dashboard scope is missing: ${table}`);
      }
    } finally {
      scope.finalize();
    }
  }

  const relationships = sqlite.prepare(`
    SELECT CASE WHEN
      EXISTS (
        SELECT 1
        FROM sessions AS child
        JOIN workers AS parent ON parent.fp = child.worker_fp
        WHERE child.dashboard_id <> parent.dashboard_id
      )
      OR EXISTS (
        SELECT 1
        FROM workspaces AS child
        JOIN workers AS parent ON parent.fp = child.worker_fp
        WHERE child.dashboard_id <> parent.dashboard_id
      )
      OR EXISTS (
        SELECT 1
        FROM workspace_sessions AS child
        JOIN workspaces AS parent ON parent.id = child.workspace_id
        WHERE child.dashboard_id <> parent.dashboard_id
      )
      OR EXISTS (
        SELECT 1
        FROM workspace_sessions AS child
        JOIN sessions AS parent ON parent.id = child.session_id
        WHERE child.dashboard_id <> parent.dashboard_id
      )
    THEN 1 ELSE 0 END AS invalid_count
  `);
  try {
    const row = relationships.get() as { invalid_count: number };
    if (row.invalid_count !== 0) {
      throw new Error("0024 existing dashboard relationship is inconsistent");
    }
  } finally {
    relationships.finalize();
  }

  const eligibleRelocations = sqlite.prepare(`
    SELECT count(*) AS eligible_count
    FROM bootstrap_tokens AS source
    CROSS JOIN temp.${AUTH_TENANCY_CONTEXT_TEMP_TABLE} AS context
    WHERE typeof(source.token) = 'text'
      AND substr(source.token, 1, length('roost_move_')) = 'roost_move_'
      AND length(source.token) > length('roost_move_')
      AND source.expires_at_ms > context.now_ms
      AND source.used_at_ms IS NOT NULL
      AND source.used_by_fp IS NOT NULL
      AND source.minted_by_fp IS NOT NULL
      AND (
        SELECT count(*)
        FROM authorized_keys AS exact_key
        JOIN account_devices AS exact_device
          ON exact_device.fingerprint = exact_key.fingerprint
        JOIN accounts AS exact_account
          ON exact_account.id = exact_device.account_id
        LEFT JOIN authorized_key_revocations AS exact_revocation
          ON exact_revocation.fingerprint = exact_key.fingerprint
        WHERE exact_key.fingerprint = source.minted_by_fp
          AND exact_account.status = 'active'
          AND exact_revocation.fingerprint IS NULL
      ) = 1
  `);
  let eligibleRelocationCount: number;
  try {
    const row = eligibleRelocations.get() as { eligible_count: number };
    eligibleRelocationCount = row.eligible_count;
  } finally {
    eligibleRelocations.finalize();
  }

  const select = sqlite.prepare("SELECT token FROM bootstrap_tokens");
  const insert = sqlite.prepare(`
    INSERT INTO ${ROOST_SHA256_HEX_TEMP_TABLE} (plaintext, token_hash)
    VALUES (?, ?)
  `);
  let ordinaryCount = 0;
  let stagedCount = 0;
  try {
    const rows = select.all() as Array<{ token: unknown }>;
    for (const { token } of rows) {
      if (
        typeof token === "string"
        && token.startsWith(RELOCATION_SENTINEL_PREFIX)
      ) {
        continue;
      }
      ordinaryCount += 1;
      if (
        typeof token !== "string"
        || Buffer.byteLength(token, "utf8") > MAX_MIGRATION_TOKEN_BYTES
      ) {
        throw new Error("roost_sha256_hex input must be bounded text");
      }
      const tokenHash = createHash("sha256")
        .update(token, "utf8")
        .digest("hex");
      const result = insert.run(token, tokenHash);
      if (result.changes !== 1) {
        throw new Error("roost_sha256_hex staging count mismatch");
      }
      stagedCount += 1;
    }
  } finally {
    insert.finalize();
    select.finalize();
  }
  if (stagedCount !== ordinaryCount) {
    throw new Error("roost_sha256_hex staging count mismatch");
  }
  return { ordinaryCount, eligibleRelocationCount };
}

function validateAuthTenancyCopy(
  sqlite: Database,
  expected: AuthTenancyPreflight,
): void {
  const counts = sqlite.prepare(`
    SELECT
      (SELECT count(*) FROM bootstrap_tokens) AS ordinary_count,
      (SELECT count(*) FROM coordinator_relocation_redemptions)
        AS relocation_count
  `);
  try {
    const actual = counts.get() as {
      ordinary_count: number;
      relocation_count: number;
    };
    if (
      actual.ordinary_count !== expected.ordinaryCount
      || actual.relocation_count !== expected.eligibleRelocationCount
    ) {
      throw new Error("0024 auth tenancy copy count mismatch");
    }
  } finally {
    counts.finalize();
  }
}

function clearAuthTenancyPreflight(sqlite: Database): void {
  sqlite.exec(`
    DROP TABLE IF EXISTS temp.${ROOST_SHA256_HEX_TEMP_TABLE};
    DROP TABLE IF EXISTS temp.${AUTH_TENANCY_CONTEXT_TEMP_TABLE}
  `);
}

function validateAppliedPrefix(
  migrations: readonly { name: string }[],
  appliedRows: readonly { name: string }[],
): void {
  for (const [idx, { name }] of appliedRows.entries()) {
    if (migrations[idx]?.name === name) continue;
    throw new Error(
      "Applied migration history is not an exact prefix of embedded migrations: "
      + `found ${name} at position ${idx + 1}`,
    );
  }
}

export async function runMigrations(
  sqlite: Database,
  embedded?: { name: string; sql: string }[],
  beforeApply?: (pendingNames: readonly string[]) => Promise<void>,
  beforeMigration?: (name: string) => Promise<void> | void,
): Promise<void> {
  assertNoOpenTransaction(sqlite);
  enableAndVerifyForeignKeys(sqlite);

  sqlite.exec(`
    CREATE TABLE IF NOT EXISTS _migrations (
      name        TEXT    PRIMARY KEY,
      applied_at  INTEGER NOT NULL
    )
  `);

  const migrations = [...(embedded && embedded.length > 0
    ? embedded
    : await loadMigrationsFromDisk())]
    .sort((left, right) => left.name.localeCompare(right.name));

  const appliedStatement = sqlite.prepare("SELECT name FROM _migrations");
  let appliedRows: { name: string }[];
  try {
    appliedRows = appliedStatement.all() as { name: string }[];
  } finally {
    appliedStatement.finalize();
  }
  appliedRows.sort((left, right) => left.name.localeCompare(right.name));
  validateAppliedPrefix(migrations, appliedRows);
  const applied = new Set(appliedRows.map(({ name }) => name));

  const pending = migrations.filter(({ name }) => !applied.has(name));
  if (pending.length > 0 && beforeApply) {
    await beforeApply(pending.map(({ name }) => name));
    assertNoOpenTransaction(sqlite);
    enableAndVerifyForeignKeys(sqlite);
  }

  for (const [pendingIndex, { name, sql }] of pending.entries()) {
    // This runs outside the migration transaction so callers can perform their
    // own atomic prerequisite cutover immediately before one named migration.
    if (beforeMigration) await beforeMigration(name);
    assertNoOpenTransaction(sqlite);
    enableAndVerifyForeignKeys(sqlite);
    const isFinalPendingMigration = pendingIndex === pending.length - 1;
    const runsAuthTenancyPreflight = name === AUTH_TENANCY_MIGRATION
      && sql.includes(`temp.${ROOST_SHA256_HEX_TEMP_TABLE}`);
    let authTenancyPreflight: AuthTenancyPreflight | undefined;
    let ownsTransaction = false;
    try {
      sqlite.exec("BEGIN");
      ownsTransaction = true;
      if (runsAuthTenancyPreflight) {
        authTenancyPreflight = stageAuthTenancyPreflight(sqlite);
      }
      sqlite.exec(sql);
      if (name === WORKER_TOMBSTONES_MIGRATION || isFinalPendingMigration) {
        validateForeignKeys(
          sqlite,
          name === WORKER_TOMBSTONES_MIGRATION ? "0025" : name,
        );
      }
      if (isFinalPendingMigration) validateIntegrity(sqlite, name);
      if (authTenancyPreflight) {
        validateAuthTenancyCopy(sqlite, authTenancyPreflight);
        clearAuthTenancyPreflight(sqlite);
      }
      const recordMigration = sqlite.prepare("INSERT INTO _migrations (name, applied_at) VALUES (?, ?)");
      try {
        recordMigration.run(name, Date.now());
      } finally {
        recordMigration.finalize();
      }
      sqlite.exec("COMMIT");
      ownsTransaction = false;
      log.info("migrate", "migration_applied", { name });
    } catch (err) {
      if (ownsTransaction && sqlite.inTransaction) {
        try { sqlite.exec("ROLLBACK"); } catch { /* preserve the original error */ }
      }
      if (runsAuthTenancyPreflight) {
        try { clearAuthTenancyPreflight(sqlite); } catch { /* rollback normally removed it */ }
      }
      log.error("migrate", "migration_failed", { name, error: String(err) });
      throw new Error(`migration failed: ${name}: ${err instanceof Error ? err.message : String(err)}`);
    }
  }
}

async function loadMigrationsFromDisk(): Promise<{ name: string; sql: string }[]> {
  const files = (await readdir(MIGRATIONS_DIR))
    .filter((f) => f.endsWith(".sql"))
    .sort();
  const out: { name: string; sql: string }[] = [];
  for (const file of files) {
    out.push({ name: file.replace(/\.sql$/, ""), sql: await readFile(join(MIGRATIONS_DIR, file), "utf8") });
  }
  return out;
}
