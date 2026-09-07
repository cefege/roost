// Pins migration 0029: the Web Push VAPID keypair is coordinator-global, so a
// dashboard-scoped copy blocks self-hosted tenancy admission. Covers the
// repair, its idempotence, deterministic promotion, and the app_settings shapes
// 0029 deliberately leaves for the tenancy guard to refuse.
// Depends on the on-disk migrations and in-memory SQLite only.
import { Database } from "bun:sqlite";
import { expect, test } from "bun:test";
import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { runMigrations } from "../src/db/migrate.ts";
import { ensureSelfHostedTenant } from "../src/self-hosted-tenant.ts";
import {
  captureInvariantError,
  INVARIANT_PREFIX,
  seedExistingTopology,
} from "./self-hosted-tenant-fixtures.ts";

const MIGRATION_NAME = "0029_global_push_vapid_identity";
const migrationsDir = join(import.meta.dir, "../migrations");
const priorMigrations = readdirSync(migrationsDir)
  .filter((name) => name.endsWith(".sql") && name < `${MIGRATION_NAME}.sql`)
  .sort()
  .map((name) => ({
    name: name.slice(0, -4),
    sql: readFileSync(join(migrationsDir, name), "utf8"),
  }));
const migration = {
  name: MIGRATION_NAME,
  sql: readFileSync(join(migrationsDir, `${MIGRATION_NAME}.sql`), "utf8"),
};

interface SettingRow {
  dashboard_id: string | null;
  value: string;
}

async function databaseBefore0029(): Promise<Database> {
  const sqlite = new Database(":memory:");
  sqlite.exec("PRAGMA foreign_keys = ON");
  await runMigrations(sqlite, priorMigrations);
  return sqlite;
}

function insertSetting(
  sqlite: Database,
  dashboardId: string | null,
  key: string,
  value: string,
  updatedAtMs: number,
): void {
  const statement = sqlite.prepare(`
    INSERT INTO app_settings (dashboard_id, key, value, updated_at_ms)
    VALUES (?, ?, ?, ?)
  `);
  try {
    statement.run(dashboardId, key, value, updatedAtMs);
  } finally {
    statement.finalize();
  }
}

function settingRows(sqlite: Database, key: string): SettingRow[] {
  const statement = sqlite.prepare(`
    SELECT dashboard_id, value FROM app_settings WHERE key = ? ORDER BY rowid
  `);
  try {
    return statement.all(key) as SettingRow[];
  } finally {
    statement.finalize();
  }
}

test("0029 drops the unreachable scoped VAPID copy and admits self-hosted tenancy", async () => {
  const sqlite = await databaseBefore0029();
  try {
    seedExistingTopology(sqlite);
    insertSetting(sqlite, null, "push.vapid", "global-identity", 200);
    insertSetting(sqlite, "dashboard-existing", "push.vapid", "stale-identity", 100);
    insertSetting(sqlite, "dashboard-existing", "ui.tab_bar_at_top", "true", 100);

    expect(captureInvariantError(
      () => ensureSelfHostedTenant(sqlite, { backfillLegacyScopes: false }),
    )).toBe(`${INVARIANT_PREFIX}app_settings contains invalid dashboard scope`);

    await runMigrations(sqlite, [...priorMigrations, migration]);

    expect(settingRows(sqlite, "push.vapid")).toEqual([
      { dashboard_id: null, value: "global-identity" },
    ]);
    expect(settingRows(sqlite, "ui.tab_bar_at_top")).toEqual([
      { dashboard_id: "dashboard-existing", value: "true" },
    ]);
    expect(ensureSelfHostedTenant(sqlite, { backfillLegacyScopes: false })).toEqual({
      accountId: "account-existing",
      organizationId: "organization-existing",
      dashboardId: "dashboard-existing",
    });

    sqlite.exec(migration.sql);
    expect(settingRows(sqlite, "push.vapid")).toEqual([
      { dashboard_id: null, value: "global-identity" },
    ]);
  } finally {
    sqlite.close(true);
  }
});

test("0029 promotes the only scoped VAPID identity instead of discarding it", async () => {
  const sqlite = await databaseBefore0029();
  try {
    seedExistingTopology(sqlite);
    insertSetting(sqlite, "dashboard-existing", "push.vapid", "only-identity", 100);

    await runMigrations(sqlite, [...priorMigrations, migration]);

    expect(settingRows(sqlite, "push.vapid")).toEqual([
      { dashboard_id: null, value: "only-identity" },
    ]);
    expect(ensureSelfHostedTenant(sqlite, { backfillLegacyScopes: false }).dashboardId)
      .toBe("dashboard-existing");
  } finally {
    sqlite.close(true);
  }
});

test("0029 keeps the newest identity when several dashboards each retained one", async () => {
  const sqlite = await databaseBefore0029();
  try {
    seedExistingTopology(sqlite);
    const secondDashboard = sqlite.prepare(`
      INSERT INTO dashboards (id, organization_id, slug, name, status, created_at_ms)
      VALUES ('dashboard-second', 'organization-existing', 'second', 'Second', 'active', 1)
    `);
    try {
      secondDashboard.run();
    } finally {
      secondDashboard.finalize();
    }
    insertSetting(sqlite, "dashboard-existing", "push.vapid", "older-identity", 100);
    insertSetting(sqlite, "dashboard-second", "push.vapid", "newer-identity", 300);

    await runMigrations(sqlite, [...priorMigrations, migration]);

    expect(settingRows(sqlite, "push.vapid")).toEqual([
      { dashboard_id: null, value: "newer-identity" },
    ]);
  } finally {
    sqlite.close(true);
  }
});

test("0029 leaves every other invalid app_settings scope to the tenancy guard", async () => {
  const sqlite = await databaseBefore0029();
  try {
    seedExistingTopology(sqlite);
    insertSetting(sqlite, null, "push.vapid", "global-identity", 200);
    insertSetting(sqlite, null, "ui.tab_bar_at_top", "true", 100);

    await runMigrations(sqlite, [...priorMigrations, migration]);

    expect(settingRows(sqlite, "ui.tab_bar_at_top")).toEqual([
      { dashboard_id: null, value: "true" },
    ]);
    expect(captureInvariantError(
      () => ensureSelfHostedTenant(sqlite, { backfillLegacyScopes: false }),
    )).toBe(`${INVARIANT_PREFIX}app_settings contains missing or foreign dashboard scope`);
  } finally {
    sqlite.close(true);
  }
});
