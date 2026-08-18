import { afterEach, beforeEach, describe, expect, test, vi } from "bun:test";
import { create } from "@bufbuild/protobuf";
import {
  AgentStatus,
  AgentStatusUpdate,
  SessionEvent,
  asSessionId,
  asWorkerFp,
  type AgentStatusUpdate as AgentStatusUpdateValue,
} from "@roost/shared/wire";
import {
  CoordWorkerUpSchema,
  WAgentStatusSchema,
  WHelloSchema,
} from "@roost/shared/proto/worker_transport_pb";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { openDb, type KyselyDB } from "../src/db/connection.ts";
import { runMigrations } from "../src/db/migrate.ts";
import {
  getAgentStatusSnapshot,
  handleWorkerAgentStatus,
  startAgentStatusHub,
  stopAgentStatusHub,
} from "../src/agent-status-hub.ts";
import { agentStatusBus, sessionBus } from "../src/buses.ts";
import { cacheSessionWorker, evictSessionWorker } from "../src/byte-hub.ts";
import { startSyncFeed } from "../src/connect/sync-feed.ts";
import { makeWorkerConn, type WorkerServiceDeps } from "../src/connect/worker-conn.ts";
import type { ConnectDeps } from "../src/connect/router.ts";

const SID = asSessionId("11111111-1111-4111-8111-111111111111");
const WORKER = asWorkerFp("a1".repeat(32));
const OTHER_WORKER = asWorkerFp("b2".repeat(32));
const cleanupDirs: string[] = [];

function status(overrides: Partial<AgentStatusUpdateValue> = {}): AgentStatusUpdateValue {
  return AgentStatusUpdate.parse({
    session_id: SID,
    agent_id: "omp",
    state: "working",
    revision: 1,
    completed_revision: 0,
    updated_at: 1_780_000_000_000,
    active: true,
    ...overrides,
  });
}

beforeEach(() => {
  stopAgentStatusHub();
  startAgentStatusHub();
  cacheSessionWorker(SID, WORKER, 7);
});

afterEach(async () => {
  stopAgentStatusHub();
  evictSessionWorker(SID);
  vi.useRealTimers();
  await Promise.all(cleanupDirs.splice(0).map((path) => rm(path, { recursive: true, force: true })));
});

describe("coordinator agent status hub", () => {
  test("retains active state and rejects stale or equal revisions", () => {
    const published: AgentStatusUpdateValue[] = [];
    const unsubscribe = agentStatusBus.subscribe((update) => published.push(update));
    try {
      expect(handleWorkerAgentStatus(WORKER, status())).toBe("accepted");
      expect(handleWorkerAgentStatus(WORKER, status({ state: "blocked" }))).toBe("stale");
      expect(handleWorkerAgentStatus(WORKER, status({ revision: 0 }))).toBe("stale");
      expect(getAgentStatusSnapshot()).toEqual([AgentStatus.parse(status())]);
      expect(published).toHaveLength(1);
    } finally {
      unsubscribe();
    }
  });

  test("publishes inactive deletion and keeps its revision floor", () => {
    expect(handleWorkerAgentStatus(WORKER, status())).toBe("accepted");
    expect(handleWorkerAgentStatus(WORKER, status({ revision: 2, active: false }))).toBe("accepted");
    expect(getAgentStatusSnapshot()).toHaveLength(0);
    expect(handleWorkerAgentStatus(WORKER, status({ revision: 1 }))).toBe("stale");
  });

  test("rejects invalid, unknown-session, and cross-worker claims", () => {
    expect(handleWorkerAgentStatus(WORKER, { ...status(), state: "finished" })).toBe("invalid");
    evictSessionWorker(SID);
    expect(handleWorkerAgentStatus(WORKER, status())).toBe("unknown-session");
    cacheSessionWorker(SID, OTHER_WORKER, 7);
    expect(handleWorkerAgentStatus(WORKER, status())).toBe("wrong-worker");
  });

  test("clears retained state when the terminal closes", () => {
    expect(handleWorkerAgentStatus(WORKER, status({ revision: 8 }))).toBe("accepted");
    const published: AgentStatusUpdateValue[] = [];
    const unsubscribe = agentStatusBus.subscribe((update) => published.push(update));
    try {
      sessionBus.publish(SessionEvent.parse({
        kind: "closed",
        session_id: SID,
        exit_code: 0,
        ts: 1_780_000_000_001,
      }));
      expect(getAgentStatusSnapshot()).toHaveLength(0);
      expect(published).toHaveLength(1);
      expect(published[0]).toMatchObject({ session_id: SID, revision: 9, active: false });
    } finally {
      unsubscribe();
    }
  });

  test("delays, cancels, and replaces push transitions", async () => {
    vi.useFakeTimers();
    stopAgentStatusHub();
    const deliveries: Array<{ sessionId: string; kind: "blocked" | "done" }> = [];
    // The injected dispatcher never reads db; only its identity is required by
    // the production dependency contract in this timer-focused test.
    const db = {} as KyselyDB;
    startAgentStatusHub({
      db,
      dispatchPush: async (_db, sessionId, kind) => {
        deliveries.push({ sessionId, kind });
      },
    });

    expect(handleWorkerAgentStatus(WORKER, status())).toBe("accepted");
    expect(handleWorkerAgentStatus(WORKER, status({
      revision: 2,
      state: "blocked",
      message: "Approval needed",
    }))).toBe("accepted");
    vi.advanceTimersByTime(999);
    expect(deliveries).toHaveLength(0);
    vi.advanceTimersByTime(1);
    await Promise.resolve();
    expect(deliveries).toEqual([{ sessionId: SID, kind: "blocked" }]);

    expect(handleWorkerAgentStatus(WORKER, status({
      revision: 3,
      state: "working",
    }))).toBe("accepted");
    expect(handleWorkerAgentStatus(WORKER, status({
      revision: 4,
      state: "blocked",
    }))).toBe("accepted");
    expect(handleWorkerAgentStatus(WORKER, status({
      revision: 5,
      state: "working",
    }))).toBe("accepted");
    vi.advanceTimersByTime(1_000);
    await Promise.resolve();
    expect(deliveries).toHaveLength(1);

    expect(handleWorkerAgentStatus(WORKER, status({
      revision: 6,
      state: "blocked",
    }))).toBe("accepted");
    expect(handleWorkerAgentStatus(WORKER, status({
      revision: 7,
      state: "idle",
      completed_revision: 1,
    }))).toBe("accepted");
    vi.advanceTimersByTime(1_000);
    await Promise.resolve();
    expect(deliveries.at(-1)).toEqual({ sessionId: SID, kind: "done" });
  });

  test("seeds Sync and fans out live updates and deletion", async () => {
    expect(handleWorkerAgentStatus(WORKER, status())).toBe("accepted");
    const dir = await mkdtemp(join(tmpdir(), "roost-agent-sync-"));
    cleanupDirs.push(dir);
    const opened = openDb(join(dir, "coord.db"));
    await runMigrations(opened.sqlite);
    // startSyncFeed reads only db; the remaining router dependencies belong to
    // unrelated RPC handlers and are deliberately absent in this focused test.
    const deps = { db: opened.db } as unknown as ConnectDeps;
    const syncFrames: Parameters<Parameters<typeof startSyncFeed>[2]>[0][] = [];
    const pairSeeded = Promise.withResolvers<void>();
    const feed = startSyncFeed(deps, 0, (frame) => {
      syncFrames.push(frame);
      if (
        frame.frame.case === "pairRequestDelta"
        && frame.frame.value.kind.case === "snapshot"
      ) pairSeeded.resolve();
    });
    try {
      const seeded = syncFrames.find((frame) => frame.frame.case === "agentStatus");
      expect(seeded?.frame).toMatchObject({
        case: "agentStatus",
        value: { sessionId: SID, agentId: "omp", state: "working", revision: 1n, active: true },
      });
      expect(handleWorkerAgentStatus(WORKER, status({ revision: 2, state: "blocked" }))).toBe("accepted");
      expect(syncFrames.at(-1)?.frame).toMatchObject({
        case: "agentStatus",
        value: { sessionId: SID, state: "blocked", revision: 2n, active: true },
      });
      expect(handleWorkerAgentStatus(WORKER, status({ revision: 3, active: false }))).toBe("accepted");
      expect(syncFrames.at(-1)?.frame).toMatchObject({
        case: "agentStatus",
        value: { sessionId: SID, revision: 3n, active: false },
      });
    } finally {
      await pairSeeded.promise;
      feed.dispose();
      opened.sqlite.close();
    }
  });

  test("decodes authenticated worker transport frames", async () => {
    vi.useFakeTimers();
    const dir = await mkdtemp(join(tmpdir(), "roost-agent-worker-frame-"));
    cleanupDirs.push(dir);
    const opened = openDb(join(dir, "coord.db"));
    await runMigrations(opened.sqlite);
    // makeWorkerConn uses only these methods in the hello/status path.
    const deps = {
      db: opened.db,
      coordKey: { verifyingKeyB64: () => "key", verifyingKeyKid: () => "kid" },
    } as unknown as WorkerServiceDeps;
    const conn = makeWorkerConn(deps, { fingerprint: WORKER }, () => 1, () => {});
    try {
      await conn.handleUpstream(create(CoordWorkerUpSchema, {
        frame: { case: "hello", value: create(WHelloSchema, { workerFp: WORKER, version: "test" }) },
      }));
      await conn.handleUpstream(create(CoordWorkerUpSchema, {
        frame: { case: "agentStatus", value: create(WAgentStatusSchema, {
          sessionId: SID,
          agentId: "omp",
          state: "blocked",
          message: "Approval needed",
          revision: 4n,
          completedRevision: 0n,
          updatedAt: 1_780_000_000_004,
          active: true,
        }) },
      }));
      expect(getAgentStatusSnapshot()).toEqual([
        AgentStatus.parse(status({
          state: "blocked",
          message: "Approval needed",
          revision: 4,
          updated_at: 1_780_000_000_004,
        })),
      ]);
    } finally {
      conn.close();
      opened.sqlite.close();
    }
  });
});
