// Agent-status transport tests pin optional identity decoding from authenticated
// worker protobufs and exact retained/live Sync projection. Hub admission rules
// stay in the focused hub and identity-order suites.

import { afterEach, beforeEach, expect, test, vi } from "bun:test";
import { create } from "@bufbuild/protobuf";
import {
  AgentOccupantId,
  AgentStatus,
  AgentStatusUpdate,
  StatusEpoch,
  asSessionId,
  asWorkerFp,
  type AgentStatusUpdate as AgentStatusUpdateValue,
} from "@roost/shared/wire";
import {
  CoordWorkerUpSchema,
  WAgentStatusSchema,
  WHelloSchema,
} from "@roost/shared/proto/worker_transport_pb";
import { SyncDomain } from "@roost/shared/proto/sync_pb";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import {
  getAgentStatusSnapshot,
  handleWorkerAgentStatus,
  startAgentStatusHub,
  stopAgentStatusHub,
} from "../src/agent-status-hub.ts";
import { cacheSessionWorker, evictSessionWorker } from "../src/byte-hub.ts";
import { titleBus } from "../src/buses.ts";
import { loadSyncResourceIndex, startSyncFeed } from "../src/connect/sync-feed.ts";
import { agentStatusFrame } from "../src/connect/sync-feed-frames.ts";
import { makeWorkerConn, type WorkerServiceDeps } from "../src/connect/worker-conn.ts";
import { connectWorkers } from "../src/connect/worker-registry.ts";
import { openDb } from "../src/db/connection.ts";
import { runMigrations } from "../src/db/migrate.ts";
import { ensureSelfHostedTenant } from "../src/self-hosted-tenant.ts";
import type { ConnectDeps } from "../src/connect/router.ts";
import {
  decodedFrames,
  flushMicrotasks,
  makeHarness,
} from "./sync-ws-v2-scheduler-harness.ts";

const SESSION_ID = asSessionId("11111111-1111-4111-8111-111111111111");
const WORKER_FP = asWorkerFp("a1".repeat(32));
const STATUS_EPOCH = StatusEpoch.parse("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa");
const OCCUPANT_ID = AgentOccupantId.parse("11111111-aaaa-4aaa-8aaa-111111111111");
const OCCUPANT_B = AgentOccupantId.parse("22222222-aaaa-4aaa-8aaa-222222222222");
const cleanupDirs: string[] = [];

function status(overrides: Partial<AgentStatusUpdateValue> = {}): AgentStatusUpdateValue {
  return AgentStatusUpdate.parse({
    session_id: SESSION_ID,
    agent_id: "omp",
    state: "working",
    revision: 1,
    completed_revision: 0,
    updated_at: 1_780_000_000_000,
    active: true,
    ...overrides,
  });
}

function identifiedStatus(
  overrides: Partial<AgentStatusUpdateValue> = {},
): AgentStatusUpdateValue {
  return status({
    status_epoch: STATUS_EPOCH,
    occupant_id: OCCUPANT_ID,
    source: "integration",
    ...overrides,
  });
}

beforeEach(() => {
  stopAgentStatusHub();
  startAgentStatusHub();
  cacheSessionWorker(SESSION_ID, WORKER_FP, 7);
});

afterEach(async () => {
  stopAgentStatusHub();
  evictSessionWorker(SESSION_ID);
  vi.useRealTimers();
  await Promise.all(cleanupDirs.splice(0).map((path) => rm(path, {
    recursive: true,
    force: true,
  })));
});

async function openStatusFeed() {
  const dir = await mkdtemp(join(tmpdir(), "roost-agent-sync-"));
  cleanupDirs.push(dir);
  const opened = openDb(join(dir, "coord.db"));
  await runMigrations(opened.sqlite);
  const tenant = ensureSelfHostedTenant(opened.sqlite, { backfillLegacyScopes: false });
  const now = Date.now();
  await opened.db.insertInto("workers").values({
    fp: WORKER_FP,
    dashboard_id: tenant.dashboardId,
    label: "agent-status-worker",
    os: "linux",
    git_sha: null,
    host_metrics_json: null,
    registered_at_ms: now,
    last_seen_ms: now,
  }).execute();
  await opened.db.insertInto("sessions").values({
    id: SESSION_ID,
    dashboard_id: tenant.dashboardId,
    worker_fp: WORKER_FP,
    channel: 7,
    kind: "shell",
    cwd: "/tmp",
    status: "open",
    created_at: now,
  }).execute();
  return {
    opened,
    scope: await loadSyncResourceIndex(opened.db),
    deps: {
      db: opened.db,
      selfHostedTenant: tenant,
    } as unknown as ConnectDeps,
  };
}

test("legacy Sync status leaves every identity field absent", () => {
  const frame = agentStatusFrame(status());
  if (frame.frame.case !== "agentStatus") throw new Error("wrong frame kind");
  expect(frame.frame.value.statusEpoch).toBeUndefined();
  expect(frame.frame.value.occupantId).toBeUndefined();
  expect(frame.frame.value.source).toBeUndefined();
});

test("retained and live Sync status preserve exact identity and source", async () => {
  expect(handleWorkerAgentStatus(WORKER_FP, identifiedStatus())).toBe("accepted");
  const { opened, scope, deps } = await openStatusFeed();
  const frames: Parameters<Parameters<typeof startSyncFeed>[3]>[0][] = [];
  const feed = startSyncFeed(deps, scope, 0, (frame) => { frames.push(frame); }, null, false);
  try {
    await feed.seeded;
    expect(frames.find((frame) => frame.frame.case === "agentStatus")?.frame).toMatchObject({
      case: "agentStatus",
      value: {
        statusEpoch: STATUS_EPOCH,
        occupantId: OCCUPANT_ID,
        source: "integration",
        revision: 1n,
        active: true,
      },
    });

    expect(handleWorkerAgentStatus(WORKER_FP, identifiedStatus({
      revision: 2,
      state: "blocked",
      source: "screen",
    }))).toBe("accepted");
    expect(frames.at(-1)?.frame).toMatchObject({
      case: "agentStatus",
      value: {
        statusEpoch: STATUS_EPOCH,
        occupantId: OCCUPANT_ID,
        source: "screen",
        revision: 2n,
        active: true,
      },
    });

    expect(handleWorkerAgentStatus(WORKER_FP, identifiedStatus({
      revision: 3,
      state: "blocked",
      active: false,
      source: "screen",
    }))).toBe("accepted");
    expect(frames.at(-1)?.frame).toMatchObject({
      case: "agentStatus",
      value: {
        statusEpoch: STATUS_EPOCH,
        occupantId: OCCUPANT_ID,
        source: "screen",
        revision: 3n,
        active: false,
      },
    });
  } finally {
    feed.dispose();
    await opened.close();
  }
});

test("subscription cutover coalesces buffered occupant replacement to the retained status", async () => {
  const { opened, scope, deps } = await openStatusFeed();
  const harness = makeHarness("agent-status-cutover", false);
  const feed = startSyncFeed(
    deps,
    scope,
    0,
    (frame, meta) => { harness.scheduler.enqueueV2Frame(harness.ws, frame, meta); },
    null,
    false,
    { version: 2, socketId: harness.socket.data.v2!.socketId, onRecoveryReset() {} },
  );
  try {
    titleBus.publish({
      session_id: SESSION_ID,
      title: "before replacement",
    });
    expect(handleWorkerAgentStatus(WORKER_FP, identifiedStatus())).toBe("accepted");
    expect(handleWorkerAgentStatus(WORKER_FP, identifiedStatus({
      revision: 2,
      active: false,
    }))).toBe("accepted");
    expect(handleWorkerAgentStatus(WORKER_FP, identifiedStatus({
      occupant_id: OCCUPANT_B,
      revision: 1,
      state: "blocked",
    }))).toBe("accepted");
    titleBus.publish({
      session_id: SESSION_ID,
      title: "after replacement",
    });
    expect(harness.terminal.queue.filter(
      (item) => item.frame.frame.case === "agentStatus",
    )).toHaveLength(3);

    harness.terminal.ready = true;
    const seeded = feed.seedDomain(SyncDomain.TERMINAL, new Set([SESSION_ID]));
    const queuedStatuses = harness.terminal.queue.flatMap((item) =>
      item.frame.frame.case === "agentStatus" ? [item.frame.frame.value] : []
    );
    expect(queuedStatuses).toMatchObject([{
      occupantId: OCCUPANT_B,
      revision: 1n,
      active: true,
    }]);
    expect(harness.terminal.queue.flatMap((item) =>
      item.frame.frame.case === "terminalTitle" ? [item.frame.frame.value.title] : []
    )).toEqual(["before replacement", "after replacement"]);

    await seeded;
    await flushMicrotasks();
    const deliveredStatuses = decodedFrames(harness.socket).flatMap((frame) =>
      frame.frame.case === "agentStatus" ? [frame.frame.value] : []
    );
    expect(deliveredStatuses).toMatchObject([{
      occupantId: OCCUPANT_B,
      revision: 1n,
      active: true,
    }]);
    expect(deliveredStatuses.some((status) => !status.active)).toBe(false);
  } finally {
    feed.dispose();
    await opened.close();
  }
});

test("authenticated worker decoding rejects partial identity and preserves a full triple", async () => {
  vi.useFakeTimers();
  const dir = await mkdtemp(join(tmpdir(), "roost-agent-worker-frame-"));
  cleanupDirs.push(dir);
  const opened = openDb(join(dir, "coord.db"));
  await runMigrations(opened.sqlite);
  const deps = {
    db: opened.db,
    selfHostedTenant: ensureSelfHostedTenant(opened.sqlite, {
      backfillLegacyScopes: false,
    }),
  } as unknown as WorkerServiceDeps;
  const conn = makeWorkerConn(
    deps,
    { fingerprint: WORKER_FP },
    () => 1,
    () => {},
  );
  try {
    await conn.handleUpstream(create(CoordWorkerUpSchema, {
      frame: {
        case: "hello",
        value: create(WHelloSchema, { workerFp: WORKER_FP, version: "test" }),
      },
    }));
    const worker = connectWorkers.get(WORKER_FP);
    if (!worker) throw new Error("hello did not claim worker generation");
    worker.ready = true;
    cacheSessionWorker(SESSION_ID, WORKER_FP, 7);

    await conn.handleUpstream(create(CoordWorkerUpSchema, {
      frame: { case: "agentStatus", value: create(WAgentStatusSchema, {
        sessionId: SESSION_ID,
        agentId: "omp",
        state: "working",
        revision: 3n,
        updatedAt: 1_780_000_000_003,
        active: true,
        statusEpoch: STATUS_EPOCH,
      }) },
    }));
    expect(getAgentStatusSnapshot()).toEqual([]);

    await conn.handleUpstream(create(CoordWorkerUpSchema, {
      frame: { case: "agentStatus", value: create(WAgentStatusSchema, {
        sessionId: SESSION_ID,
        agentId: "omp",
        state: "blocked",
        message: "Approval needed",
        revision: 4n,
        updatedAt: 1_780_000_000_004,
        active: true,
        statusEpoch: STATUS_EPOCH,
        occupantId: OCCUPANT_ID,
        source: "integration",
      }) },
    }));
    expect(getAgentStatusSnapshot()).toEqual([AgentStatus.parse(identifiedStatus({
      state: "blocked",
      message: "Approval needed",
      revision: 4,
      updated_at: 1_780_000_000_004,
    }))]);
  } finally {
    conn.close();
    await opened.close();
  }
});
