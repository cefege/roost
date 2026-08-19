import { afterEach, describe, expect, test } from "bun:test";
import { mkdtempSync, rmSync, statSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { Database } from "bun:sqlite";
import { runBackup } from "../src/backup.ts";
import { runMigrations } from "../src/db/migrate.ts";

const workdirs: string[] = [];

afterEach(() => {
  for (const dir of workdirs.splice(0)) rmSync(dir, { recursive: true, force: true });
});

async function openBackup(archivePath: string): Promise<Database> {
  const path = `${archivePath}.opened.db`;
  const compressed = new Uint8Array(await Bun.file(archivePath).arrayBuffer());
  await Bun.write(path, Bun.gunzipSync(compressed));
  return new Database(path, { readonly: true });
}

describe("coordinator backups", () => {
  test("scheduled and pre-migration archives are consistent during WAL activity", async () => {
    const dir = mkdtempSync(join(tmpdir(), "roost-backup-"));
    workdirs.push(dir);
    const dbPath = join(dir, "coord.db");
    const sqlite = new Database(dbPath, { create: true });
    const reader = new Database(dbPath);
    const writer = new Database(dbPath);
    try {
      sqlite.exec("PRAGMA journal_mode=WAL");
      sqlite.exec("PRAGMA wal_autocheckpoint=0");
      sqlite.exec("CREATE TABLE sentinels (value TEXT NOT NULL)");
      reader.exec("BEGIN");
      reader.query("SELECT count(*) FROM sentinels").get();

      writer.query("INSERT INTO sentinels (value) VALUES (?)").run("scheduled-sentinel");
      expect(statSync(`${dbPath}-wal`).size).toBeGreaterThan(0);
      const scheduledPath = await runBackup(sqlite, dbPath, "scheduled");

      writer.query("INSERT INTO sentinels (value) VALUES (?)").run("pre-migration-sentinel");
      const preMigrationPath = await runBackup(sqlite, dbPath, "pre-migration");
      expect(preMigrationPath).not.toBe(scheduledPath);
      expect(statSync(join(dir, "backups")).mode & 0o777).toBe(0o700);

      for (const [archivePath, expected] of [
        [scheduledPath, ["scheduled-sentinel"]],
        [preMigrationPath, ["pre-migration-sentinel", "scheduled-sentinel"]],
      ] as const) {
        expect(statSync(archivePath).mode & 0o777).toBe(0o600);
        const backup = await openBackup(archivePath);
        try {
          expect(backup.query("PRAGMA integrity_check").get()).toEqual({ integrity_check: "ok" });
          const values = backup
            .query<{ value: string }, []>("SELECT value FROM sentinels ORDER BY value")
            .all()
            .map(({ value }) => value);
          expect(values).toEqual([...expected]);
        } finally {
          backup.close(true);
        }
      }
    } finally {
      try { reader.exec("ROLLBACK"); } catch { /* already closed or no transaction */ }
      reader.close(true);
      writer.close(true);
      sqlite.close(true);
    }
  });

  test("migration callback receives all pending names exactly once before applying", async () => {
    const sqlite = new Database(":memory:");
    const migrations = [
      { name: "0001_first", sql: "CREATE TABLE first_table (id INTEGER)" },
      { name: "0002_second", sql: "CREATE TABLE second_table (id INTEGER)" },
    ];
    const calls: string[][] = [];
    const beforeApply = async (names: readonly string[]): Promise<void> => {
      calls.push([...names]);
      expect(sqlite.query("SELECT name FROM sqlite_master WHERE name = 'first_table'").get()).toBeNull();
    };
    try {
      await runMigrations(sqlite, migrations, beforeApply);
      await runMigrations(sqlite, migrations, beforeApply);
      expect(calls).toEqual([["0001_first", "0002_second"]]);
      expect(sqlite.query("SELECT name FROM sqlite_master WHERE name = 'second_table'").get()).toEqual({
        name: "second_table",
      });
    } finally {
      sqlite.close(true);
    }
  });

  test("a pre-migration backup failure prevents every pending migration", async () => {
    const dir = mkdtempSync(join(tmpdir(), "roost-backup-failure-"));
    workdirs.push(dir);
    const dbPath = join(dir, "coord.db");
    const sqlite = new Database(dbPath, { create: true });
    writeFileSync(join(dir, "backups"), "blocks backup directory creation");
    try {
      await expect(runMigrations(
        sqlite,
        [{ name: "0001_must_not_apply", sql: "CREATE TABLE forbidden (id INTEGER)" }],
        async () => {
          await runBackup(sqlite, dbPath, "pre-migration");
        },
      )).rejects.toThrow();
      expect(sqlite.query("SELECT name FROM sqlite_master WHERE name = 'forbidden'").get()).toBeNull();
      expect(sqlite.query("SELECT name FROM _migrations WHERE name = '0001_must_not_apply'").get()).toBeNull();
    } finally {
      sqlite.close(true);
    }
  });
});
