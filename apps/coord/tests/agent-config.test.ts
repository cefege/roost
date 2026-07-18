// Default-agent (launch-button) config storage over the app_settings KV.
// Fresh db → claude default; round-trips a built-in and a custom command;
// empty selected falls back to claude. Drives the REAL get/set over an
// in-memory migrated DB (same harness as task-bus-publish.test.ts).

import { describe, test, expect, beforeAll, afterAll } from "bun:test";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { openDb, type KyselyDB } from "../src/db/connection.ts";
import { runMigrations } from "../src/db/migrate.ts";
import { getAgentConfig, setAgentConfig } from "../src/agent-config.ts";

let workdir: string;
let closeDb: () => void;
let db: KyselyDB;
beforeAll(async () => {
  workdir = mkdtempSync(join(tmpdir(), "roost-agentcfg-"));
  const opened = openDb(join(workdir, "test.db"));
  await runMigrations(opened.sqlite);
  closeDb = () => { try { opened.sqlite.close(); } catch { /* ignore */ } };
  db = opened.db;
});

afterAll(() => { closeDb?.(); rmSync(workdir, { recursive: true, force: true }); });

describe("agent-config", () => {
  test("fresh db → claude default, empty custom", async () => {
    expect(await getAgentConfig(db)).toEqual({ selected: "claude", customCommand: "", autoLaunch: false });
  });

  test("built-in selection round-trips", async () => {
    await setAgentConfig(db, { selected: "codex", customCommand: "", autoLaunch: false });
    expect((await getAgentConfig(db)).selected).toBe("codex");
  });

  test("custom command round-trips", async () => {
    await setAgentConfig(db, { selected: "custom", customCommand: "aider", autoLaunch: false });
    expect(await getAgentConfig(db)).toEqual({ selected: "custom", customCommand: "aider", autoLaunch: false });
  });

  test("empty selected falls back to claude", async () => {
    await setAgentConfig(db, { selected: "", customCommand: "", autoLaunch: false });
    expect((await getAgentConfig(db)).selected).toBe("claude");
  });

  test("auto-launch toggle round-trips", async () => {
    await setAgentConfig(db, { selected: "claude", customCommand: "", autoLaunch: true });
    expect((await getAgentConfig(db)).autoLaunch).toBe(true);
    await setAgentConfig(db, { selected: "claude", customCommand: "", autoLaunch: false });
    expect((await getAgentConfig(db)).autoLaunch).toBe(false);
  });
});
