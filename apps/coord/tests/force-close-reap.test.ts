// Force-remove offline-worker sessions: coord tombstones via a `closed` event
// when the worker is offline (sessionsKill force=true), and the snapshot
// reconcile must NOT resurrect a force-closed id when the worker returns with
// the PTY still live — instead it reaps the orphan on the now-online worker.
// Covers the split-brain guard in event-log.ts (reap + skip-upsert).

import { describe, it, expect, beforeAll, afterAll } from "bun:test";
import { existsSync, mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { openDb, type DbHandle, type KyselyDB } from "../src/db/connection.ts";
import { runMigrations } from "../src/db/migrate.ts";
import { appendEvent, type AppendEventResult } from "../src/event-log.ts";
import { __setConnectWorkerForTest } from "../src/connect/worker-service.ts";
import type { CoordWorkerDown } from "@roost/shared/proto/worker_transport_pb";
import { SessionEvent, asSessionId, asWorkerFp, asChannelId } from "@roost/shared/wire";
import type { Session } from "@roost/shared/wire";
import { sessionBus } from "../src/buses.ts";
import { lookupSessionId } from "../src/byte-hub.ts";

const ORGANIZATION_ID = "force-close-reap-organization";
const DASHBOARD_ID = "force-close-reap-dashboard";
const FP = asWorkerFp("cc".repeat(32));
const tick = () => new Promise((r) => setTimeout(r, 60));

let workdir: string;
let db: KyselyDB;
let handle: DbHandle;

function liveSession(sid: string): Session {
  return {
    id: asSessionId(sid), worker_fp: FP, channel: asChannelId(1), kind: "shell",
    cwd: "/tmp", workspace_id: null, status: "open",
    created_at: 1000, closed_at: null, custom_title: null,
  };
}
function opened(sid: string): SessionEvent {
  return SessionEvent.parse({
    kind: "opened", session_id: asSessionId(sid), worker_fp: FP,
    channel: asChannelId(1), session_kind: "shell", cwd: "/tmp", ts: 1,
  });
}
function snapshot(sids: string[]): SessionEvent {
  return { kind: "snapshot", worker_fp: FP, sessions: sids.map(liveSession), ts: 5000 };
}

beforeAll(async () => {
  workdir = mkdtempSync(join(tmpdir(), "roost-reap-"));
  handle = openDb(join(workdir, "test.db"));
  db = handle.db;
  await runMigrations(handle.sqlite);
  await db.insertInto("organizations").values({
    id: ORGANIZATION_ID,
    slug: "force-close-reap",
    name: "Force close reap",
    status: "active",
    created_at_ms: 1,
  }).execute();
  await db.insertInto("dashboards").values({
    id: DASHBOARD_ID,
    organization_id: ORGANIZATION_ID,
    slug: "force-close-reap",
    name: "Force close reap",
    status: "active",
    created_at_ms: 1,
  }).execute();
  await db.insertInto("workers").values({
    dashboard_id: DASHBOARD_ID,
    fp: FP, label: "test", os: "darwin", git_sha: null, host_metrics_json: null,
    registered_at_ms: 1, last_seen_ms: 1,
  }).execute();
});

afterAll(async () => {
  __setConnectWorkerForTest(FP, null);
  try { await handle.close(); } finally { if (existsSync(workdir)) rmSync(workdir, { recursive: true, force: true }); }
});

describe("force-close + snapshot reap", () => {
  it("force-closed session is NOT resurrected by a returning snapshot, and the orphan is reaped", async () => {
    const SID = "00000000-0000-4000-8000-000000000aa1";
    const captured: CoordWorkerDown[] = [];
    __setConnectWorkerForTest(FP, { workerFp: FP, dashboardId: DASHBOARD_ID, send: (f) => captured.push(f) });

    await appendEvent(db, opened(SID), {
      worker_fp: FP, client_seq: 1, dashboardId: DASHBOARD_ID,
    });
    // Force-close while worker was offline = a direct `closed` tombstone.
    await appendEvent(db, SessionEvent.parse({
      kind: "closed", session_id: asSessionId(SID), exit_code: null, ts: 2,
    }), {
      worker_fp: null, client_seq: null, dashboardId: DASHBOARD_ID,
    });
    expect((await db.selectFrom("sessions").select("id").where("id", "=", SID).execute()).length).toBe(0);

    // Worker returns and re-announces the PTY as live. The force-close filter
    // must run before durable JSON, projection, channel index, and publication.
    const publishedSnapshots: SessionEvent[] = [];
    const unsubscribe = sessionBus.subscribe((event) => {
      if (event.kind === "snapshot") publishedSnapshots.push(event);
    });
    let result: AppendEventResult | undefined;
    try {
      result = await appendEvent(db, snapshot([SID]), {
        worker_fp: FP, client_seq: 2, dashboardId: DASHBOARD_ID,
      });
    } finally {
      unsubscribe();
    }
    await tick();

    // Resurrection guard: row stays gone.
    const rows = await db.selectFrom("sessions").select("id").where("id", "=", SID).execute();
    expect(rows.length).toBe(0);
    expect(result!.event).toMatchObject({ kind: "snapshot", sessions: [] });
    const stored = await db.selectFrom("events")
      .select("payload_json")
      .where("worker_fp", "=", FP)
      .where("client_seq", "=", 2)
      .executeTakeFirstOrThrow();
    expect(JSON.parse(stored.payload_json)).toMatchObject({
      kind: "snapshot",
      sessions: [],
    });
    expect(publishedSnapshots).toHaveLength(1);
    expect(publishedSnapshots[0]).toMatchObject({
      kind: "snapshot",
      sessions: [],
    });
    expect(lookupSessionId(FP, asChannelId(1))).toBeUndefined();

    // Reap: a kill browser-command went to the worker for this orphan.
    const kills = captured
      .map((f) => (f.frame.case === "browserCommand" ? f.frame.value.frameJson : null))
      .filter((j): j is string => !!j)
      .map((j) => JSON.parse(j))
      .filter((m) => m.kind === "kill" && m.session_id === SID);
    expect(kills.length).toBe(1);
  });

  it("a normal live session (no prior closed event) IS installed by the snapshot — no false reap", async () => {
    const SID = "00000000-0000-4000-8000-000000000bb2";
    const captured: CoordWorkerDown[] = [];
    __setConnectWorkerForTest(FP, { workerFp: FP, dashboardId: DASHBOARD_ID, send: (f) => captured.push(f) });

    await appendEvent(db, snapshot([SID]), {
      worker_fp: FP, client_seq: 3, dashboardId: DASHBOARD_ID,
    });
    await tick();

    const rows = await db.selectFrom("sessions").select("id").where("id", "=", SID).execute();
    expect(rows.length).toBe(1); // installed, not reaped
    const kills = captured
      .map((f) => (f.frame.case === "browserCommand" ? f.frame.value.frameJson : null))
      .filter((j): j is string => !!j)
      .map((j) => JSON.parse(j))
      .filter((m) => m.kind === "kill");
    expect(kills.length).toBe(0);
  });
});
