import { describe, expect, test } from "bun:test";
import { Database } from "bun:sqlite";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { runMigrations } from "../src/db/migrate.ts";

const migrationSql = readFileSync(
  join(import.meta.dir, "../migrations/0018_authorized_key_revocations.sql"),
  "utf8",
);

function pre0018(): Database {
  const sqlite = new Database(":memory:");
  sqlite.exec(`
    CREATE TABLE authorized_keys (
      fingerprint TEXT PRIMARY KEY,
      public_key BLOB NOT NULL,
      label TEXT NOT NULL,
      added_at INTEGER NOT NULL
    );
    CREATE TABLE bootstrap_tokens (
      token TEXT PRIMARY KEY,
      kind TEXT NOT NULL,
      label TEXT NOT NULL,
      created_at_ms INTEGER NOT NULL,
      expires_at_ms INTEGER NOT NULL,
      used_at_ms INTEGER,
      used_by_fp TEXT
    );
    CREATE TABLE marker (value TEXT NOT NULL);
  `);
  return sqlite;
}

describe("authorized key revocation migration", () => {
  test("upgrades a pre-0018 database and enforces both trigger boundaries", async () => {
    const sqlite = pre0018();
    await runMigrations(sqlite, [{ name: "0018_authorized_key_revocations", sql: migrationSql }]);
    // query(), never prepare(): a prepare()d Statement is finalized only on GC and
    // keeps the handle busy, so the close(true) below would fail with "database is
    // locked". query() statements are finalized by close().
    sqlite.query(`
      INSERT INTO authorized_key_revocations
        (fingerprint, revoked_at_ms, revoked_by_fp, reason)
      VALUES (?, ?, ?, ?)
    `).run("revoked", Date.now(), "test", "test");

    sqlite.exec("BEGIN");
    sqlite.query("INSERT INTO marker (value) VALUES (?)").run("must-rollback");
    expect(() => sqlite.query(`
      INSERT INTO authorized_keys (fingerprint, public_key, label, added_at)
      VALUES (?, ?, ?, ?)
    `).run("revoked", new Uint8Array(32), "bad", Date.now())).toThrow("authorized key revoked");
    sqlite.exec("ROLLBACK");
    expect(sqlite.query("SELECT value FROM marker").all()).toEqual([]);

    expect(() => sqlite.query(`
      INSERT INTO bootstrap_tokens (
        token, kind, label, created_at_ms, expires_at_ms,
        used_at_ms, used_by_fp, minted_by_fp
      ) VALUES (?, 'browser', 'bad', 0, 1, NULL, NULL, ?)
    `).run("token", "revoked")).toThrow("bootstrap minter revoked");
    expect(sqlite.query("SELECT token FROM bootstrap_tokens").all()).toEqual([]);
    sqlite.close(true);
  });

  test("a failed script rolls back and is not recorded", async () => {
    const sqlite = pre0018();
    await expect(runMigrations(sqlite, [{
      name: "0018_broken",
      sql: "CREATE TABLE should_rollback (id INTEGER); THIS IS NOT SQL;",
    }])).rejects.toThrow("migration failed");
    expect(sqlite.query("SELECT name FROM _migrations WHERE name = ?").get("0018_broken")).toBeNull();
    expect(() => sqlite.query("SELECT * FROM should_rollback").all()).toThrow();
    sqlite.close(true);
  });
});
