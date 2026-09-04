// Validates SQLite connection state and post-migration database invariants.
// The migration runner calls these checks before and within owned transactions.
// Every prepared statement is finalized so strict database close remains reliable.
import type { Database } from "bun:sqlite";

export function enableAndVerifyForeignKeys(sqlite: Database): void {
  sqlite.exec("PRAGMA foreign_keys = ON");
  const statement = sqlite.prepare<{ foreign_keys: number }, []>("PRAGMA foreign_keys");
  try {
    const row = statement.get();
    if (Number(row?.foreign_keys) !== 1) {
      throw new Error("SQLite foreign key enforcement is required for migrations");
    }
  } finally {
    statement.finalize();
  }
}

export function validateForeignKeys(sqlite: Database, migrationName: string): void {
  const statement = sqlite.prepare("PRAGMA foreign_key_check");
  try {
    const violation = statement.get() as {
      table: string;
      rowid: number | null;
      parent: string;
      fkid: number;
    } | null;
    if (violation) {
      throw new Error(
        `${migrationName} foreign key check failed: ${violation.table} -> ${violation.parent}`,
      );
    }
  } finally {
    statement.finalize();
  }
}

export function validateIntegrity(sqlite: Database, migrationName: string): void {
  const statement = sqlite.prepare<{ integrity_check: string }, []>("PRAGMA integrity_check");
  try {
    const rows = statement.all();
    if (rows.length !== 1 || rows[0]?.integrity_check !== "ok") {
      throw new Error(
        `${migrationName} integrity check failed: ${rows[0]?.integrity_check ?? "no result"}`,
      );
    }
  } finally {
    statement.finalize();
  }
}

export function assertNoOpenTransaction(sqlite: Database): void {
  if (sqlite.inTransaction) {
    throw new Error("Migration runner cannot use an open SQLite transaction");
  }
}
