// A migration that shipped and was later removed leaves a real history row in
// every database old enough to have applied it. Pins that the runner accepts
// such a row — including when its slot number was reused — and still applies
// the rest of the chain. Exercises runMigrations against apps/coord/migrations.
import { expect, test } from "bun:test";
import { mkdtempSync, readdirSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { openDb } from "../src/db/connection.ts";
import { runMigrations } from "../src/db/migrate.ts";

const RETIRED_MIGRATION_NAME = "0017_agent_ui_frames";
const REUSED_SLOT_MIGRATION_NAME = "0017_retire_structured_agent_sessions";

const MIGRATIONS_DIR = join(import.meta.dir, "../migrations");

function allMigrations(): Array<{ name: string; sql: string }> {
  return readdirSync(MIGRATIONS_DIR)
    .filter((file) => file.endsWith(".sql"))
    .sort()
    .map((file) => ({
      name: file.slice(0, -4),
      sql: readFileSync(join(MIGRATIONS_DIR, file), "utf8"),
    }));
}

function appliedNames(sqlite: { query: (sql: string) => { all: () => unknown[] } }): string[] {
  return (sqlite.query("SELECT name FROM _migrations ORDER BY name").all() as Array<
    { name: string }
  >).map((row) => row.name);
}

test("a removed migration's history row does not fail the prefix check", async () => {
  const workdir = mkdtempSync(join(tmpdir(), "roost-retired-migration-"));
  const opened = openDb(join(workdir, "test.db"));
  const { sqlite } = opened;
  const migrations = allMigrations();
  const head = migrations.at(-1)!.name;
  try {
    const throughReusedSlot = migrations.filter(
      ({ name }) => name <= REUSED_SLOT_MIGRATION_NAME,
    );
    await runMigrations(sqlite, throughReusedSlot);
    // The state a coordinator installed before the removal is in: the retired
    // name sorts BEFORE the migration that reused its slot, so an ordinal
    // comparison over the raw history diverges at that position.
    sqlite.run("INSERT INTO _migrations (name, applied_at) VALUES (?, ?)", [
      RETIRED_MIGRATION_NAME,
      1,
    ]);

    await runMigrations(sqlite, migrations);

    const applied = appliedNames(sqlite);
    expect(applied).toContain(RETIRED_MIGRATION_NAME);
    expect(applied).toContain(head);
    for (const { name } of migrations) expect(applied).toContain(name);
  } finally {
    opened.db.destroy();
    sqlite.close();
    rmSync(workdir, { recursive: true, force: true });
  }
});

test("an unknown history row still fails closed", async () => {
  const workdir = mkdtempSync(join(tmpdir(), "roost-unknown-migration-"));
  const opened = openDb(join(workdir, "test.db"));
  const { sqlite } = opened;
  const migrations = allMigrations();
  try {
    await runMigrations(sqlite, migrations);
    sqlite.run("INSERT INTO _migrations (name, applied_at) VALUES (?, ?)", [
      "0002_not_a_real_migration",
      1,
    ]);
    await expect(runMigrations(sqlite, migrations)).rejects.toThrow(
      "not an exact prefix",
    );
  } finally {
    opened.db.destroy();
    sqlite.close();
    rmSync(workdir, { recursive: true, force: true });
  }
});
