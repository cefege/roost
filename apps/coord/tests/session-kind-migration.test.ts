import { expect, test } from "bun:test";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { openDb } from "../src/db/connection.ts";
import { runMigrations } from "../src/db/migrate.ts";

const WORKER_FP = "aa".repeat(32);
const MIGRATION_NAME = "0015_normalize_legacy_session_kinds";
const RETIRE_MIGRATION_NAME = "0017_retire_structured_agent_sessions";

test("normalizes open legacy PTY kinds while preserving closed history", async () => {
  const workdir = mkdtempSync(join(tmpdir(), "roost-session-kind-migration-"));
  const opened = openDb(join(workdir, "test.db"));
  const { sqlite } = opened;
  try {
    await runMigrations(sqlite);
    sqlite.run(
      "INSERT INTO workers (fp, label, os, git_sha, host_metrics_json, registered_at_ms, last_seen_ms, reachable_addr, keeper_stale) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
      [WORKER_FP, "test", "darwin", null, null, 1, 1, null, null],
    );
    sqlite.run(
      "INSERT INTO sessions (id, worker_fp, channel, kind, cwd, workspace_id, status, agent_json, created_at, closed_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
      ["open-agent", WORKER_FP, 1, "agent", "/tmp", null, "open", null, 1, null],
    );
    sqlite.run(
      "INSERT INTO sessions (id, worker_fp, channel, kind, cwd, workspace_id, status, agent_json, created_at, closed_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
      ["open-claude", WORKER_FP, 2, "claude", "/tmp", null, "open", null, 1, null],
    );
    sqlite.run(
      "INSERT INTO sessions (id, worker_fp, channel, kind, cwd, workspace_id, status, agent_json, created_at, closed_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
      ["closed-agent", WORKER_FP, 3, "agent", "/tmp", null, "closed", null, 1, 2],
    );
    sqlite.run("DELETE FROM _migrations WHERE name = ?", [MIGRATION_NAME]);

    const sql = await Bun.file(join(import.meta.dir, "../migrations/0015_normalize_legacy_session_kinds.sql")).text();
    await runMigrations(sqlite, [{ name: MIGRATION_NAME, sql }]);

    const rows = sqlite.query("SELECT id, kind FROM sessions ORDER BY id").all() as { id: string; kind: string }[];
    expect(rows).toEqual([
      { id: "closed-agent", kind: "agent" },
      { id: "open-agent", kind: "shell" },
      { id: "open-claude", kind: "shell" },
    ]);
  } finally {
    try { await opened.close(); } finally { rmSync(workdir, { recursive: true, force: true }); }
  }
});

test("retires structured sessions before normalizing all history to shell", async () => {
  const workdir = mkdtempSync(join(tmpdir(), "roost-retire-agent-session-"));
  const opened = openDb(join(workdir, "test.db"));
  const { sqlite } = opened;
  try {
    await runMigrations(sqlite);
    sqlite.run(
      "INSERT INTO workers (fp, label, os, git_sha, host_metrics_json, registered_at_ms, last_seen_ms, reachable_addr, keeper_stale) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
      [WORKER_FP, "test", "darwin", null, null, 1, 1, null, null],
    );
    sqlite.run(
      "UPDATE _migrations SET applied_at = ? WHERE name = ?",
      [1_000, MIGRATION_NAME],
    );
    sqlite.run("DELETE FROM _migrations WHERE name = ?", [RETIRE_MIGRATION_NAME]);

    const insertSession = sqlite.query(
      "INSERT INTO sessions (id, worker_fp, channel, kind, cwd, workspace_id, status, agent_json, created_at, closed_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    );
    insertSession.run("equal-timestamp-open-agent", WORKER_FP, 1, "agent", "/tmp", null, "open", null, 1_000, null);
    insertSession.run(
      "structured-open-agent",
      WORKER_FP,
      2,
      "agent",
      "/tmp",
      null,
      "open",
      '{"session_file":"/tmp/history.jsonl"}',
      1_001,
      null,
    );
    insertSession.run("closed-agent", WORKER_FP, 3, "agent", "/tmp", null, "closed", null, 1_001, 7);
    insertSession.run("open-claude", WORKER_FP, 4, "claude", "/tmp", null, "open", null, 1_001, null);

    const sql = await Bun.file(
      join(import.meta.dir, "../migrations/0017_retire_structured_agent_sessions.sql"),
    ).text();
    await runMigrations(sqlite, [{ name: RETIRE_MIGRATION_NAME, sql }]);

    const rows = sqlite.query(
      "SELECT id, kind, status, closed_at, agent_json FROM sessions ORDER BY id",
    ).all() as Array<{
      id: string;
      kind: string;
      status: string;
      closed_at: number | null;
      agent_json: string | null;
    }>;
    expect(rows[0]?.id).toBe("closed-agent");
    expect(rows[0]?.kind).toBe("shell");
    expect(rows[0]?.status).toBe("closed");
    expect(rows[0]?.closed_at).toBe(7);
    expect(rows[0]?.agent_json).toBeNull();
    expect(rows[1]?.id).toBe("equal-timestamp-open-agent");
    expect(rows[1]?.kind).toBe("shell");
    expect(rows[1]?.status).toBe("closed");
    expect(Number.isInteger(rows[1]?.closed_at)).toBe(true);
    expect((rows[1]?.closed_at ?? 0) > 0).toBe(true);
    expect(rows[1]?.agent_json).toBeNull();
    expect(rows[2]).toEqual({
      id: "open-claude", kind: "shell", status: "open", closed_at: null, agent_json: null,
    });
    expect(rows[3]?.id).toBe("structured-open-agent");
    expect(rows[3]?.kind).toBe("shell");
    expect(rows[3]?.status).toBe("closed");
    expect(Number.isInteger(rows[3]?.closed_at)).toBe(true);
    expect((rows[3]?.closed_at ?? 0) > 0).toBe(true);
    expect(rows[3]?.agent_json).toBe('{"session_file":"/tmp/history.jsonl"}');
  } finally {
    try { await opened.close(); } finally { rmSync(workdir, { recursive: true, force: true }); }
  }
});
