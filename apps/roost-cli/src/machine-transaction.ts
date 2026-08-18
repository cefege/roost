import { randomUUID } from "node:crypto";
import { mkdir } from "node:fs/promises";
import { join, win32 } from "node:path";
import { Database } from "bun:sqlite";
import { applyPrivateDacl, type DurabilityOptions } from "./durability.ts";
import { roostServiceDir, type PathEnv } from "./paths.ts";
import { supportedHostPlatform, type SupportedHostPlatform } from "./platform.ts";
import {
  windowsPrepareUpdaterArtifact,
  windowsProtectUpdaterArtifact,
} from "./windows-helper.ts";

export type MachineTransactionKind = "install" | "update" | "relocation" | "keeper-refresh" | "deploy";

export interface MachineTransactionRecord {
  schemaVersion: 1;
  kind: MachineTransactionKind;
  journalPath: string;
  ownerPid: number;
  processEpoch: string;
  acquiredAt: string;
}

export interface AcquireMachineTransactionOptions extends DurabilityOptions {
  platform?: SupportedHostPlatform;
  lockPath?: string;
  processEpoch?: string;
  env?: PathEnv;
}

export interface MachineTransactionLock extends MachineTransactionRecord {
  lockPath: string;
  release(): Promise<void>;
}

export class MachineTransactionBusyError extends Error {
  readonly active: MachineTransactionRecord | null;
  constructor(lockPath: string, active: MachineTransactionRecord | null) {
    super(`machine transaction already active at ${lockPath}${active ? ` (${active.kind})` : ""}`);
    this.name = "MachineTransactionBusyError";
    this.active = active;
  }
}

function isSqliteBusy(error: unknown): boolean {
  const candidate = error as { code?: string | number; message?: string };
  return candidate.code === "SQLITE_BUSY"
    || candidate.code === 5
    || /\b(?:database is locked|SQLITE_BUSY)\b/i.test(candidate.message ?? String(error));
}

function closeQuietly(database: Database): void {
  try {
    database.close();
  } catch {
    // The acquisition error remains authoritative.
  }
}

/**
 * Hold one OS-backed SQLite write transaction for the complete machine
 * mutation. SQLite's kernel file lock is atomic, releases automatically on
 * process death, and cannot be confused by PID reuse or a partially written
 * owner record. This is intentionally a lock database, not a durable journal;
 * each caller's journalPath identifies the separate recovery record.
 */
export async function acquireMachineTransaction(
  kind: MachineTransactionKind,
  journalPath: string,
  options: AcquireMachineTransactionOptions = {},
): Promise<MachineTransactionLock> {
  const platform = options.platform ?? supportedHostPlatform();
  const env = options.env ?? process.env;
  const base = roostServiceDir(env, platform);
  const lockPath = options.lockPath ?? (platform === "win32"
    ? win32.join(base, "machine-transaction.sqlite")
    : join(base, "machine-transaction.sqlite"));
  await mkdir(base, { recursive: true, mode: 0o700 });
  // Windows mutation entry points are brokered by RoostUpdaterV2. Establish
  // the updater-private lock inode before SQLite opens it; never create a
  // base-account-writable database and repair it after parsing.
  if (platform === "win32") {
    await windowsPrepareUpdaterArtifact(lockPath, "private", options.helper);
  }

  const record: MachineTransactionRecord = {
    schemaVersion: 1,
    kind,
    journalPath,
    ownerPid: process.pid,
    processEpoch: options.processEpoch ?? randomUUID(),
    acquiredAt: new Date().toISOString(),
  };
  const database = new Database(lockPath, { create: true, strict: true });
  let transactionOpen = false;
  try {
    database.exec("PRAGMA busy_timeout = 0");
    database.exec("PRAGMA journal_mode = DELETE");
    database.exec("PRAGMA synchronous = FULL");
    database.exec(`
      CREATE TABLE IF NOT EXISTS active_machine_transaction (
        singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
        record_json TEXT NOT NULL
      )
    `);
    if (platform === "win32") {
      await windowsProtectUpdaterArtifact(lockPath, "private", options.helper);
    } else {
      await applyPrivateDacl(lockPath, { ...options, platform });
    }
    database.exec("BEGIN EXCLUSIVE");
    transactionOpen = true;
    const deployLeaseTable = database.query(
      "SELECT name FROM sqlite_schema WHERE type = 'table' AND name = 'deploy_lease'",
    ).get();
    if (deployLeaseTable) {
      let leases: Array<{
        singleton?: unknown;
        owner?: unknown;
        created_at?: unknown;
        expires_at?: unknown;
      }>;
      try {
        leases = database.query(
          "SELECT singleton, owner, created_at, expires_at FROM deploy_lease",
        ).all() as typeof leases;
      } catch {
        throw new MachineTransactionBusyError(lockPath, null);
      }
      if (leases.length > 1) {
        throw new MachineTransactionBusyError(lockPath, null);
      }
      const lease = leases[0];
      if (lease) {
        const valid = lease.singleton === 1
          && typeof lease.owner === "string"
          && lease.owner.length > 0
          && Number.isSafeInteger(lease.created_at)
          && (lease.created_at as number) >= 0
          && Number.isSafeInteger(lease.expires_at)
          && (lease.expires_at as number) >= (lease.created_at as number);
        if (!valid || (lease.expires_at as number) > Math.floor(Date.now() / 1000)) {
          throw new MachineTransactionBusyError(lockPath, null);
        }
        database.query("DELETE FROM deploy_lease WHERE singleton = 1").run();
      }
    }
    database.query(
      "INSERT OR REPLACE INTO active_machine_transaction (singleton, record_json) VALUES (1, ?)",
    ).run(JSON.stringify(record));
  } catch (error) {
    if (transactionOpen) {
      try {
        database.exec("ROLLBACK");
      } catch {
        // Closing releases the kernel lock even if rollback itself failed.
      }
    }
    closeQuietly(database);
    if (isSqliteBusy(error)) throw new MachineTransactionBusyError(lockPath, null);
    throw error;
  }

  let released = false;
  return {
    ...record,
    lockPath,
    async release() {
      if (released) return;
      try {
        database.query("DELETE FROM active_machine_transaction WHERE singleton = 1").run();
        database.exec("COMMIT");
        transactionOpen = false;
        released = true;
      } catch (error) {
        if (transactionOpen) {
          try {
            database.exec("ROLLBACK");
          } catch {
            // close() below still releases the OS file lock.
          }
          transactionOpen = false;
        }
        throw error;
      } finally {
        closeQuietly(database);
      }
    },
  };
}
