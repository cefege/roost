// Default-agent launcher configuration over app_settings.

import { describe, test, expect, beforeAll, afterAll } from "bun:test";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { openDb, type KyselyDB } from "../src/db/connection.ts";
import { runMigrations } from "../src/db/migrate.ts";
import { getAgentConfig, setAgentConfig } from "../src/agent-config.ts";
const ORGANIZATION_ID = "organization-agent-config";
const DASHBOARD_ID = "dashboard-agent-config";

let workdir: string;
let closeDb: () => Promise<void>;
let db: KyselyDB;
beforeAll(async () => {
  workdir = mkdtempSync(join(tmpdir(), "roost-agentcfg-"));
  const opened = openDb(join(workdir, "test.db"));
  await runMigrations(opened.sqlite);
  closeDb = async () => { await opened.close(); };
  db = opened.db;
  const now = Date.now();
  await db.insertInto("organizations").values({
    id: ORGANIZATION_ID,
    slug: "agent-config-organization",
    name: "Agent Config Organization",
    status: "active",
    created_at_ms: now,
  }).execute();
  await db.insertInto("dashboards").values({
    id: DASHBOARD_ID,
    organization_id: ORGANIZATION_ID,
    slug: "agent-config",
    name: "Agent Config",
    status: "active",
    created_at_ms: now,
  }).execute();
});

afterAll(async () => { await closeDb?.(); rmSync(workdir, { recursive: true, force: true }); });

describe("agent-config", () => {
  test("fresh db → OMP default, empty custom", async () => {
    expect(await getAgentConfig(db, DASHBOARD_ID)).toEqual({ selected: "omp", customCommand: "", autoLaunch: false });
  });

  test("built-in selection round-trips", async () => {
    await setAgentConfig(db, DASHBOARD_ID, { selected: "codex", customCommand: "", autoLaunch: false });
    expect((await getAgentConfig(db, DASHBOARD_ID)).selected).toBe("codex");
  });

  test("custom command round-trips", async () => {
    await setAgentConfig(db, DASHBOARD_ID, { selected: "custom", customCommand: "aider", autoLaunch: false });
    expect(await getAgentConfig(db, DASHBOARD_ID)).toEqual({ selected: "custom", customCommand: "aider", autoLaunch: false });
  });

  test("empty selected falls back to OMP", async () => {
    await setAgentConfig(db, DASHBOARD_ID, { selected: "", customCommand: "", autoLaunch: false });
    expect((await getAgentConfig(db, DASHBOARD_ID)).selected).toBe("omp");
  });

  test("auto-launch toggle round-trips", async () => {
    await setAgentConfig(db, DASHBOARD_ID, { selected: "omp", customCommand: "", autoLaunch: true });
    expect((await getAgentConfig(db, DASHBOARD_ID)).autoLaunch).toBe(true);
    await setAgentConfig(db, DASHBOARD_ID, { selected: "omp", customCommand: "", autoLaunch: false });
    expect((await getAgentConfig(db, DASHBOARD_ID)).autoLaunch).toBe(false);
  });
});
