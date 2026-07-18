// Kysely<DB> singleton. Opens bun:sqlite Database at path, wraps with
// BunSqliteDialect. WAL mode + busy timeout applied for write concurrency.
// Caller (main.ts) must call close() on shutdown.

import { Kysely } from "kysely";
// Direct ESM import to avoid the CJS shim which fails with kysely's ESM-only build.
import { BunSqliteDialect } from "kysely-bun-sqlite/dist/index.js";
import { Database } from "bun:sqlite";
import type { DB } from "./schema.ts";

export type KyselyDB = Kysely<DB>;

export interface DbHandle {
  db: KyselyDB;
  sqlite: Database;
}

export function openDb(dbPath: string): DbHandle {
  const sqlite = new Database(dbPath, { create: true });
  // WAL mode: concurrent readers don't block writers.
  sqlite.exec("PRAGMA journal_mode=WAL");
  // 5s busy timeout: prevents SQLITE_BUSY under light write contention.
  sqlite.exec("PRAGMA busy_timeout=5000");
  // FK enforcement: cascade deletes work on workspace_sessions.
  sqlite.exec("PRAGMA foreign_keys=ON");

  const db = new Kysely<DB>({
    dialect: new BunSqliteDialect({ database: sqlite }),
  });

  return { db, sqlite };
}
