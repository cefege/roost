// D-4b: appendEvent dedup on (worker_fp, client_seq).

import { describe, it, expect, beforeAll, afterAll } from "bun:test";
import { existsSync, mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { openDb, type DbHandle, type KyselyDB } from "../src/db/connection.ts";
import { runMigrations } from "../src/db/migrate.ts";
import { appendEvent } from "../src/event-log.ts";
import { SessionEvent, asSessionId, asWorkerFp, asChannelId } from "@roost/shared/wire";

let workdir: string;
let db: KyselyDB;
let handle: DbHandle;

beforeAll(async () => {
  workdir = mkdtempSync(join(tmpdir(), "roost-d4b-"));
  handle = openDb(join(workdir, "test.db"));
  db = handle.db;
  await runMigrations(handle.sqlite);
  // Register worker rows so `opened` events satisfy the FK on sessions.worker_fp.
  for (const fp of ["aa".repeat(32), "bb".repeat(32)]) {
    await db.insertInto("workers").values({
      fp, label: "test", os: "darwin", git_sha: null, host_metrics_json: null,
      registered_at_ms: 1, last_seen_ms: 1,
    }).execute();
  }
});

afterAll(async () => {
  try { await handle.close(); } finally { if (existsSync(workdir)) rmSync(workdir, { recursive: true, force: true }); }
});

describe("appendEvent dedup (D-4b)", () => {
  it("first delivery of (worker_fp, client_seq) inserts; second is no-op", async () => {
    const wfp = "aa".repeat(32);
    const ev = SessionEvent.parse({
      kind: "opened",
      session_id: asSessionId("00000000-0000-4000-8000-000000000010"),
      worker_fp: asWorkerFp(wfp),
      channel: asChannelId(1),
      session_kind: "shell",
      cwd: "/tmp",
      ts: 1,
    });

    await appendEvent(db, ev, { worker_fp: wfp, client_seq: 42 });
    await appendEvent(db, ev, { worker_fp: wfp, client_seq: 42 });

    const rows = await db.selectFrom("events")
      .selectAll()
      .where("worker_fp", "=", wfp)
      .where("client_seq", "=", 42)
      .execute();
    expect(rows.length).toBe(1);
  });

  it("same worker_fp with different client_seq inserts both", async () => {
    const wfp = "bb".repeat(32);
    const mk = (sid: string) => SessionEvent.parse({
      kind: "opened",
      session_id: asSessionId(sid),
      worker_fp: asWorkerFp(wfp),
      channel: asChannelId(1),
      session_kind: "shell",
      cwd: "/tmp",
      ts: 1,
    });
    await appendEvent(db, mk("00000000-0000-4000-8000-000000000011"), { worker_fp: wfp, client_seq: 1 });
    await appendEvent(db, mk("00000000-0000-4000-8000-000000000012"), { worker_fp: wfp, client_seq: 2 });

    const rows = await db.selectFrom("events")
      .select("client_seq")
      .where("worker_fp", "=", wfp)
      .where("client_seq", "is not", null)
      .orderBy("client_seq")
      .execute();
    expect(rows.map(r => r.client_seq)).toEqual([1, 2]);
  });

  it("null worker_fp / client_seq bypasses the dedup index", async () => {
    // Synthetic ghost-close case — no worker_fp, no client_seq.
    const ev = SessionEvent.parse({
      kind: "closed",
      session_id: asSessionId("00000000-0000-4000-8000-000000000013"),
      exit_code: null,
      ts: 1,
    });
    // Ghost close on a non-existent session is allowed by appendEvent
    // (it warns "session_not_found" but inserts the event row). Both
    // calls should land — no dedup.
    await appendEvent(db, ev);
    await appendEvent(db, ev);
    const rows = await db.selectFrom("events")
      .selectAll()
      .where("session_id", "=", "00000000-0000-4000-8000-000000000013")
      .execute();
    expect(rows.length).toBe(2);
  });
});
