// Custom migration runner. Reads *.sql files from apps/coord/migrations/
// in lex order, tracks applied set in _migrations table.
// Does NOT use Kysely's Migrator (subpath import issue in bun bundler mode);
// uses raw bun:sqlite Database instead. CRITICAL: throws on any failure.
// R0.9.

import { readdir, readFile } from "node:fs/promises";
import { join, resolve, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import type { Database } from "bun:sqlite";
import { log } from "@roost/shared/log";

const MIGRATIONS_DIR = resolve(
  dirname(fileURLToPath(import.meta.url)),
  "../../migrations",
);

export async function runMigrations(
  sqlite: Database,
  embedded?: { name: string; sql: string }[],
): Promise<void> {

  // Ensure _migrations table exists before we query it.
  sqlite.exec(`
    CREATE TABLE IF NOT EXISTS _migrations (
      name        TEXT    PRIMARY KEY,
      applied_at  INTEGER NOT NULL
    )
  `);

  // Compiled `roost` binary passes the SQL inlined (gen-embed.ts, type:"text")
  // because the on-disk migrations dir isn't present; from-source reads
  // apps/coord/migrations/*.sql in lex order.
  const migrations = embedded && embedded.length > 0
    ? embedded
    : await loadMigrationsFromDisk();

  // bun:sqlite .all() is untyped; this query literally selects `name`.
  const appliedRows = sqlite.prepare("SELECT name FROM _migrations").all() as { name: string }[];
  const applied = new Set(appliedRows.map((r) => r.name));

  for (const { name, sql } of migrations) {
    if (applied.has(name)) continue;

    // Strip -- line comments FIRST so semicolons inside comments don't
    // create spurious statement splits. Then split on ; and skip empties.
    const stripped = sql
      .split("\n")
      .map((l) => {
        const commentIdx = l.indexOf("--");
        return commentIdx >= 0 ? l.slice(0, commentIdx) : l;
      })
      .join("\n");
    const stmts = stripped
      .split(";")
      .map((s) => s.trim())
      .filter((s) => s.length > 0);

    // Execute each statement. Use exec() for DDL; run() for parameterized.
    try {
      sqlite.exec("BEGIN");
      for (const stmt of stmts) {
        sqlite.run(stmt);
      }
      sqlite.run(`INSERT INTO _migrations (name, applied_at) VALUES ('${name.replace(/'/g, "''")}', ${Date.now()})`);
      sqlite.exec("COMMIT");
      console.log(JSON.stringify({ ev: "migration_applied", name }));
    } catch (err) {
      try { sqlite.exec("ROLLBACK"); } catch { /* ignore if no tx */ }
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
