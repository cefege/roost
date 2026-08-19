// Default-agent launcher configuration over app_settings.

import { describe, test, expect, beforeAll, afterAll } from "bun:test";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { openDb, type KyselyDB } from "../src/db/connection.ts";
import { runMigrations } from "../src/db/migrate.ts";
import { getAgentConfig, setAgentConfig } from "../src/agent-config.ts";

let workdir: string;
let closeDb: () => Promise<void>;
let db: KyselyDB;
beforeAll(async () => {
  workdir = mkdtempSync(join(tmpdir(), "roost-agentcfg-"));
  const opened = openDb(join(workdir, "test.db"));
  await runMigrations(opened.sqlite);
  closeDb = async () => { await opened.close(); };
  db = opened.db;
});

afterAll(async () => { await closeDb?.(); rmSync(workdir, { recursive: true, force: true }); });

describe("agent-config", () => {
  test("fresh db → OMP default, empty custom", async () => {
    expect(await getAgentConfig(db)).toEqual({ selected: "omp", customCommand: "", autoLaunch: false });
  });

  test("built-in selection round-trips", async () => {
    await setAgentConfig(db, { selected: "codex", customCommand: "", autoLaunch: false });
    expect((await getAgentConfig(db)).selected).toBe("codex");
  });

  test("custom command round-trips", async () => {
    await setAgentConfig(db, { selected: "custom", customCommand: "aider", autoLaunch: false });
    expect(await getAgentConfig(db)).toEqual({ selected: "custom", customCommand: "aider", autoLaunch: false });
  });

  test("empty selected falls back to OMP", async () => {
    await setAgentConfig(db, { selected: "", customCommand: "", autoLaunch: false });
    expect((await getAgentConfig(db)).selected).toBe("omp");
  });

  test("auto-launch toggle round-trips", async () => {
    await setAgentConfig(db, { selected: "omp", customCommand: "", autoLaunch: true });
    expect((await getAgentConfig(db)).autoLaunch).toBe(true);
    await setAgentConfig(db, { selected: "omp", customCommand: "", autoLaunch: false });
    expect((await getAgentConfig(db)).autoLaunch).toBe(false);
  });
});
