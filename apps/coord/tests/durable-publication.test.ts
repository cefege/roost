// Durable publication + channel-index lifecycle (coordinator half of the
// terminal-stream-stall repair). Every contract below is an ORDERING or
// EXACTNESS fact a browser depends on, and each one had a real failure mode:
//
//   - appendEvent published on sessionBus from INSIDE its transaction, so a tab
//     could apply `opened`/`respawned`/`snapshot` before coord installed the
//     matching (worker_fp, channel)→session route — the first claim/keystroke
//     after a respawn was written into a channel no keeper owned.
//   - `respawned` never touched the channel index at all (the retired bus hook
//     only knew opened/closed/snapshot), so cells and PTY metadata for the new
//     keeper channel were dropped as unmapped until a browser Sync reconnect.
//   - a worker `snapshot` primed ADDITIVELY, so a session the worker no longer
//     ran kept its pre-restart route, and the SQLite breadcrumb re-cached it.
//   - a superseded worker socket kept mutating coordinator state, letting a
//     late exact snapshot replace its own replacement's index.

import { describe, test, expect, beforeEach, afterAll } from "bun:test";
import { existsSync, mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { create } from "@bufbuild/protobuf";
import {
  CoordWorkerUpSchema, WHelloSchema, WSessionEventSchema, WCellGridSchema,
} from "@roost/shared/proto/worker_transport_pb";
import { PbCellGridFrameSchema } from "@roost/shared/proto/cell_pb";
import { openDb, type DbHandle } from "../src/db/connection.ts";
import { runMigrations } from "../src/db/migrate.ts";
import { appendEvent } from "../src/event-log.ts";
import {
  applyDurableChannelIndex, evictSessionWorker, getCachedSessionWorker,
  isWorkerChannelIndexReconciled, lookupSessionId, publishBytes, publishCellGrid,
  resetWorkerChannelIndexReconcile,
} from "../src/byte-hub.ts";
import { globalBytesBus, globalCellBus, sessionBus } from "../src/buses.ts";
import { makeWorkerConn, type WorkerServiceDeps } from "../src/connect/worker-conn.ts";
import { connectWorkers } from "../src/connect/worker-registry.ts";
import { processInputControl, terminalViewerIdentity } from "../src/connect/session-control.ts";
import type { ConnectDeps } from "../src/connect/router.ts";
import { eventToProto } from "@roost/shared/wire/event-proto";
import { SessionEvent, asChannelId, asSessionId, asWorkerFp } from "@roost/shared/wire";
import type { Session } from "@roost/shared/wire";

const FP = asWorkerFp("d1".repeat(32));
const OTHER_FP = asWorkerFp("d2".repeat(32));
const SID_A = asSessionId("00000000-0000-4000-8000-0000000000a1");
const SID_B = asSessionId("00000000-0000-4000-8000-0000000000b1");
const SID_C = asSessionId("00000000-0000-4000-8000-0000000000c1");

let workdir = "";
let writer: DbHandle;
// Independent connection on the same file: under WAL a reader only sees rows of
// COMMITTED transactions, so a row visible here proves appendEvent's tx closed.
let reader: DbHandle;
let clientSeq = 0;

function openedEvent(sid: string, channel: number, workerFp = FP): SessionEvent {
  return SessionEvent.parse({
    kind: "opened", session_id: asSessionId(sid), worker_fp: workerFp,
    channel: asChannelId(channel), session_kind: "shell", cwd: "/tmp", ts: 1,
  });
}

function respawnedEvent(sid: string, channel: number): SessionEvent {
  return SessionEvent.parse({
    kind: "respawned", session_id: asSessionId(sid),
    new_channel: asChannelId(channel), ts: 2,
  });
}

function liveSession(sid: string, channel: number, workerFp = FP): Session {
  return {
    id: asSessionId(sid), worker_fp: workerFp, channel: asChannelId(channel),
    kind: "shell", cwd: "/tmp", workspace_id: null, status: "open",
    created_at: 1000, closed_at: null, custom_title: null,
  };
}

function snapshotEvent(sessions: Session[], workerFp = FP): SessionEvent {
  return { kind: "snapshot", worker_fp: workerFp, sessions, ts: 5000 };
}

// Distinct client_seq per append: the (worker_fp, client_seq) unique index would
// otherwise dedupe a replay and deliberately skip publication.
function append(event: SessionEvent, workerFp: string | null = FP): Promise<void> {
  clientSeq += 1;
  return appendEvent(writer.db, event, {
    worker_fp: workerFp,
    client_seq: workerFp === null ? null : clientSeq,
  });
}

function committedChannel(sid: string): number | null {
  const row = reader.sqlite.query("select channel from sessions where id = ?").get(sid);
  if (!row || typeof row !== "object" || !("channel" in row)) return null;
  return typeof row.channel === "number" ? row.channel : null;
}

function cellFrame(seq: number) {
  return create(PbCellGridFrameSchema, {
    cols: 80, rows: 24, full: true, seq: BigInt(seq), gridEpoch: `epoch:${seq}`,
  });
}

beforeEach(async () => {
  if (!workdir) {
    workdir = mkdtempSync(join(tmpdir(), "roost-durable-pub-"));
    const dbPath = join(workdir, "coord.db");
    writer = openDb(dbPath);
    await runMigrations(writer.sqlite);
    for (const fp of [FP, OTHER_FP]) {
      await writer.db.insertInto("workers").values({
        fp, label: "test", os: "linux", git_sha: null, host_metrics_json: null,
        registered_at_ms: 1, last_seen_ms: 1,
      }).execute();
    }
    reader = openDb(dbPath);
  }
  // Only this file's fingerprints: connectWorkers is process-wide.
  connectWorkers.delete(FP);
  connectWorkers.delete(OTHER_FP);
  for (const sid of [SID_A, SID_B, SID_C]) {
    applyDurableChannelIndex(
      SessionEvent.parse({ kind: "closed", session_id: asSessionId(sid), exit_code: 0, ts: 9 }),
      null,
    );
  }
  // Every test starts in the pre-reconcile window (a fresh worker generation).
  resetWorkerChannelIndexReconcile(FP);
  resetWorkerChannelIndexReconcile(OTHER_FP);
  await writer.db.deleteFrom("sessions").execute();
  await writer.db.deleteFrom("events").execute();
});

afterAll(async () => {
  connectWorkers.delete(FP);
  connectWorkers.delete(OTHER_FP);
  try { await reader?.close(); } finally { // a leak in one handle must not skip the other close or the rm
    try { await writer?.close(); } finally { if (workdir && existsSync(workdir)) rmSync(workdir, { recursive: true, force: true }); }
  }
});

describe("appendEvent durable publication order", () => {
  test("no sessionBus publication precedes commit + exact channel binding", async () => {
    interface Observation {
      kind: string;
      boundNew: string | undefined;
      boundOld: string | undefined;
      cachedChannel: number | undefined;
      committedChannel: number | null;
    }
    const seen: Observation[] = [];
    const unsub = sessionBus.subscribe((ev) => {
      seen.push({
        kind: ev.kind,
        boundNew: lookupSessionId(FP, asChannelId(ev.kind === "opened" ? 11 : 12)),
        boundOld: ev.kind === "respawned" ? lookupSessionId(FP, asChannelId(11)) : undefined,
        cachedChannel: getCachedSessionWorker(SID_A)?.channel,
        committedChannel: committedChannel(SID_A),
      });
    });
    try {
      await append(openedEvent(SID_A, 11));
      await append(respawnedEvent(SID_A, 12));
    } finally {
      unsub();
    }

    expect(seen).toEqual([
      // The `opened` subscriber already saw the committed row AND its route.
      { kind: "opened", boundNew: SID_A, boundOld: undefined, cachedChannel: 11, committedChannel: 11 },
      // The `respawned` subscriber sees the new route bound and the old one gone.
      { kind: "respawned", boundNew: SID_A, boundOld: undefined, cachedChannel: 12, committedChannel: 12 },
    ]);
  });

  test("a deduped replay publishes nothing and leaves the index untouched", async () => {
    await append(openedEvent(SID_A, 11));
    const published: string[] = [];
    const unsub = sessionBus.subscribe((ev) => published.push(ev.kind));
    try {
      // Same (worker_fp, client_seq) as the append above → dedupe hit.
      await appendEvent(writer.db, respawnedEvent(SID_A, 12), {
        worker_fp: FP, client_seq: clientSeq,
      });
    } finally {
      unsub();
    }
    expect(published).toEqual([]);
    expect(lookupSessionId(FP, asChannelId(12))).toBeUndefined();
    expect(getCachedSessionWorker(SID_A)?.channel).toBe(11);
  });

  test("an unknown-session event neither publishes nor binds", async () => {
    const published: string[] = [];
    const unsub = sessionBus.subscribe((ev) => published.push(ev.kind));
    try {
      await append(respawnedEvent(SID_C, 44));
    } finally {
      unsub();
    }
    expect(published).toEqual([]);
    expect(lookupSessionId(FP, asChannelId(44))).toBeUndefined();
    expect(getCachedSessionWorker(SID_C)).toBeUndefined();
  });

  test("respawned without an authenticated worker never infers one from the cache", async () => {
    await append(openedEvent(SID_A, 11));
    // A producer with no fingerprint cannot bind the new channel: guessing the
    // worker from the route cache could bind on a worker already replaced.
    await append(respawnedEvent(SID_A, 12), null);
    expect(lookupSessionId(FP, asChannelId(12))).toBeUndefined();
    expect(lookupSessionId(FP, asChannelId(11))).toBe(SID_A);
  });
});

describe("respawned channel-index operation", () => {
  test("routes cells and PTY metadata on the new channel immediately, old channel dead", async () => {
    await append(openedEvent(SID_A, 11));

    const cells: Array<{ sessionId: string; seq: string }> = [];
    const bytes: string[] = [];
    const unsubCells = globalCellBus.subscribe((f) => {
      if (f.sessionId === SID_A) cells.push({ sessionId: f.sessionId, seq: f.seq.toString() });
    });
    const unsubBytes = globalBytesBus.subscribe((m) => {
      if (m.session_id === SID_A) bytes.push(new TextDecoder().decode(m.bytes));
    });
    try {
      publishCellGrid(FP, asChannelId(11), cellFrame(1));
      expect(cells).toEqual([{ sessionId: SID_A, seq: "1" }]);

      await append(respawnedEvent(SID_A, 12));

      // No browser reconnect, no re-`opened`: the new channel is already live.
      publishCellGrid(FP, asChannelId(12), cellFrame(2));
      publishBytes(FP, asChannelId(12), new TextEncoder().encode("\x1b]0;new title\x07"));
      // The dead channel routes nothing — a surviving stale key would fan the
      // old core's trailing output into the same session.
      publishCellGrid(FP, asChannelId(11), cellFrame(3));
      publishBytes(FP, asChannelId(11), new TextEncoder().encode("stale"));
    } finally {
      unsubCells();
      unsubBytes();
    }

    expect(cells).toEqual([
      { sessionId: SID_A, seq: "1" },
      { sessionId: SID_A, seq: "2" },
    ]);
    expect(bytes).toEqual(["\x1b]0;new title\x07"]);
    expect(lookupSessionId(FP, asChannelId(11))).toBeUndefined();
    expect(lookupSessionId(FP, asChannelId(12))).toBe(SID_A);
    // Input/claim routing (getCachedSessionWorker) moves in the same step.
    expect(getCachedSessionWorker(SID_A)).toEqual({ worker_fp: FP, channel: 12 });
    expect(committedChannel(SID_A)).toBe(12);
  });

  test("a later close for the respawned session prunes exactly the new binding", async () => {
    await append(openedEvent(SID_A, 11));
    await append(respawnedEvent(SID_A, 12));
    await append(SessionEvent.parse({
      kind: "closed", session_id: SID_A, exit_code: 0, ts: 3,
    }));
    expect(lookupSessionId(FP, asChannelId(12))).toBeUndefined();
    expect(getCachedSessionWorker(SID_A)).toBeUndefined();
  });
});

describe("exact worker snapshot reconciliation", () => {
  test("removes stale/rebound live routes, keeps DB breadcrumbs", async () => {
    await append(openedEvent(SID_A, 11));
    await append(openedEvent(SID_B, 12));
    await append(openedEvent(SID_C, 13, OTHER_FP), OTHER_FP);

    // The returning worker announces only A, on a NEW channel.
    await append(snapshotEvent([liveSession(SID_A, 21)]));

    expect(lookupSessionId(FP, asChannelId(21))).toBe(SID_A);
    expect(getCachedSessionWorker(SID_A)).toEqual({ worker_fp: FP, channel: 21 });
    // Rebound: A's pre-restart channel no longer routes.
    expect(lookupSessionId(FP, asChannelId(11))).toBeUndefined();
    // Absent: B loses its live route and its input/claim route cache.
    expect(lookupSessionId(FP, asChannelId(12))).toBeUndefined();
    expect(getCachedSessionWorker(SID_B)).toBeUndefined();
    // Another worker's routes are untouched by this worker's snapshot.
    expect(lookupSessionId(OTHER_FP, asChannelId(13))).toBe(SID_C);
    expect(getCachedSessionWorker(SID_C)).toEqual({ worker_fp: OTHER_FP, channel: 13 });
    expect(isWorkerChannelIndexReconciled(FP)).toBe(true);
    expect(isWorkerChannelIndexReconciled(OTHER_FP)).toBe(false);

    // Breadcrumbs survive: the sidebar keeps showing where you were working.
    const rows = await writer.db.selectFrom("sessions").select(["id", "channel", "status"])
      .orderBy("id").execute();
    expect(rows).toEqual([
      { id: SID_A, channel: 21, status: "open" },
      { id: SID_B, channel: 12, status: "open" },
      { id: SID_C, channel: 13, status: "open" },
    ]);
  });

  test("input cannot repopulate a stale route from the open breadcrumb", async () => {
    await append(openedEvent(SID_B, 12));
    // processInputControl reads only db before route resolution; the remaining
    // router dependencies belong to unrelated RPC handlers.
    const deps = { db: writer.db } as unknown as ConnectDeps;
    const command = {
      identity: terminalViewerIdentity("f".repeat(64), "tab-1"),
      sessionId: SID_B,
      inputSeq: 1n,
      data: new TextEncoder().encode("ls\r"),
    };

    // Pre-reconcile (coord restarted under a live worker): the DB breadcrumb is
    // the only route source, so the batch reaches route resolution and fails on
    // the missing transport — NOT on an unknown session.
    evictSessionWorker(SID_B);
    const beforeReconcile = await processInputControl(deps, command);
    expect(beforeReconcile).toMatchObject({ status: "rejected", reason: "worker unavailable" });

    // After the worker declared its exact live set without B, B is offline and
    // the breadcrumb must not resurrect its pre-restart channel.
    await append(snapshotEvent([liveSession(SID_A, 21)]));
    const afterReconcile = await processInputControl(deps, { ...command, inputSeq: 2n });
    expect(afterReconcile).toMatchObject({ status: "rejected", reason: "unknown session" });
    expect(getCachedSessionWorker(SID_B)).toBeUndefined();
  });

  test("a foreign session inside a snapshot is not bound", async () => {
    await append(snapshotEvent([liveSession(SID_A, 21), liveSession(SID_B, 22, OTHER_FP)]));
    expect(lookupSessionId(FP, asChannelId(21))).toBe(SID_A);
    expect(lookupSessionId(FP, asChannelId(22))).toBeUndefined();
    expect(lookupSessionId(OTHER_FP, asChannelId(22))).toBeUndefined();
  });
});

describe("superseded worker generation fence", () => {
  function upFrame(event: SessionEvent, seq: number) {
    return create(CoordWorkerUpSchema, {
      frame: { case: "event", value: create(WSessionEventSchema, {
        event: eventToProto(event, 0)!,
        clientSeq: BigInt(seq),
      }) },
    });
  }

  test("a superseded socket cannot publish cells, append events, or replace the index", async () => {
    // makeWorkerConn uses only these members on the hello/event/cell path.
    const deps = {
      db: writer.db,
      coordKey: { verifyingKeyB64: () => "key", verifyingKeyKid: () => "kid" },
    } as unknown as WorkerServiceDeps;
    const hello = create(CoordWorkerUpSchema, {
      frame: { case: "hello", value: create(WHelloSchema, { workerFp: FP, version: "test" }) },
    });
    let closedOld = 0;
    const oldConn = makeWorkerConn(deps, { fingerprint: FP }, () => 1, () => { closedOld += 1; });
    const newConn = makeWorkerConn(deps, { fingerprint: FP }, () => 1, () => { /* current */ });
    try {
      await oldConn.handleUpstream(hello);
      expect(oldConn.isCurrentGeneration()).toBe(true);

      // A newer authenticated hello for the same fingerprint takes over.
      await newConn.handleUpstream(hello);
      expect(closedOld).toBe(1); // the prior socket is torn down, not left live
      expect(oldConn.isCurrentGeneration()).toBe(false);
      expect(newConn.isCurrentGeneration()).toBe(true);

      // The current generation installs the authoritative route.
      await newConn.handleUpstream(upFrame(openedEvent(SID_A, 11), 101));
      expect(lookupSessionId(FP, asChannelId(11))).toBe(SID_A);

      const cells: string[] = [];
      const unsub = globalCellBus.subscribe((f) => cells.push(f.sessionId));
      try {
        // Late frames from the superseded socket: an event append, an exact
        // snapshot that would wipe the replacement's index, and a cell publish.
        await oldConn.handleUpstream(upFrame(openedEvent(SID_B, 12), 102));
        await oldConn.handleUpstream(upFrame(snapshotEvent([]), 103));
        await oldConn.handleUpstream(create(CoordWorkerUpSchema, {
          frame: { case: "cellGrid", value: create(WCellGridSchema, {
            channelId: 11, frame: cellFrame(7),
          }) },
        }));
      } finally {
        unsub();
      }

      expect(cells).toEqual([]);
      expect(lookupSessionId(FP, asChannelId(11))).toBe(SID_A);
      expect(lookupSessionId(FP, asChannelId(12))).toBeUndefined();
      expect(isWorkerChannelIndexReconciled(FP)).toBe(false);
      const rows = await writer.db.selectFrom("events").select("client_seq").orderBy("id").execute();
      expect(rows.map((r) => Number(r.client_seq))).toEqual([101]);
      const sessions = await writer.db.selectFrom("sessions").select("id").execute();
      expect(sessions.map((r) => r.id)).toEqual([SID_A]);
    } finally {
      oldConn.close();
      newConn.close();
    }
  });
});
