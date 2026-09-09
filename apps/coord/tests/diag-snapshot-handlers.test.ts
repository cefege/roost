// DiagSnapshot handler coverage for normalized session filters and worker
// fan-out. A migrated single-tenant database provides durable session/worker
// scope while fake connections resolve real pending RPCs.

import { create } from "@bufbuild/protobuf";
import {
  Code,
  createContextValues,
  type HandlerContext,
} from "@connectrpc/connect";
import { afterAll, afterEach, beforeAll, beforeEach, describe, expect, test } from "bun:test";
import { DiagSnapshotRequestSchema } from "@roost/shared/proto/coordinator_pb";
import {
  WTerminalPipelineSnapshotSchema,
} from "@roost/shared/proto/worker_transport_pb";
import {
  TerminalPipelineSessionSnapshotSchema,
} from "@roost/shared/proto/wire_pb";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { callerKey } from "../src/connect/auth-interceptor.ts";
import { makeSystemHandlers } from "../src/connect/handlers-system.ts";
import { __setConnectWorkerForTest } from "../src/connect/worker-registry.ts";
import type { ConnectDeps } from "../src/connect/router.ts";
import { rejectPendingRpcsForWorker, resolvePendingRpc } from "../src/router/pending-rpcs.ts";
import { openDb, type KyselyDB } from "../src/db/connection.ts";
import { runMigrations } from "../src/db/migrate.ts";
import {
  ensureSelfHostedTenant,
  type SelfHostedTenant,
} from "../src/self-hosted-tenant.ts";

const WORKER_A = "a1b2c3d4".repeat(8);
const WORKER_C = "c3d4e5f6".repeat(8);
const WORKER_LOCAL = "d4e5f6a7".repeat(8);
const LOCAL_UNSELECTED_SESSION = "91000000-0000-4000-8000-000000000065";
const MISSING_SESSION = "91000000-0000-4000-8000-000000000066";
const BATCH_SESSION_IDS = Array.from(
  { length: 64 },
  (_, index) => `91000000-0000-4000-8000-${String(index + 1).padStart(12, "0")}`,
);


type SnapshotPayload = {
  coord: { sessions: Record<string, unknown> };
  workers: Record<string, {
    status: string;
    snapshot?: { sessions: Record<string, unknown> };
    terminal_pipeline?: {
      status: string;
      snapshot?: { sessions: Array<{ session_id: string }> };
    };
  }>;
};

let workdir = "";
let db: KyselyDB;
let closeDb: () => Promise<void>;
let tenant: SelfHostedTenant;
let sentWorkerFps: string[] = [];
let sentPipelineTargetsByWorker: Record<string, Array<{
  sessionId: string;
  viewId: string;
}>> = {};

const diagnosticSessionsByWorker: Record<string, Record<string, unknown>> = {
  [WORKER_A]: Object.fromEntries(BATCH_SESSION_IDS.slice(0, 32).map((sessionId) => [sessionId, {}])),
  [WORKER_C]: Object.fromEntries(BATCH_SESSION_IDS.slice(32).map((sessionId) => [sessionId, {}])),
  [WORKER_LOCAL]: { [LOCAL_UNSELECTED_SESSION]: {} },
};

function deviceContext(): HandlerContext {
  const values = createContextValues();
  values.set(callerKey, {
    kind: "account-device",
    fingerprint: "diag-snapshot-device",
    label: "diagnostic test device",
    accountId: tenant.accountId,
  });
  return { values, signal: new AbortController().signal } as unknown as HandlerContext;
}

function diagRequest(overrides: Partial<{
  sessionFilterId: string;
  sessionFilterIds: string[];
}> = {}) {
  return create(DiagSnapshotRequestSchema, overrides);
}

function openSession(id: string, workerFp: string, channel: number) {
  return {
    id,
    dashboard_id: tenant.dashboardId,
    worker_fp: workerFp,
    channel,
    kind: "shell" as const,
    cwd: "/tmp",
    status: "open" as const,
    created_at: 1,
  };
}

function snapshotPayload(response: { snapshotJson?: string }): SnapshotPayload {
  if (response.snapshotJson === undefined) throw new Error("DiagSnapshot response omitted snapshot JSON");
  return JSON.parse(response.snapshotJson) as SnapshotPayload;
}

function installedWorkerSnapshot(workerFp: string): Record<string, unknown> {
  return {
    captured_at_ms: 1,
    build: { git_sha: "test" },
    worker_fp: workerFp,
    sessions: diagnosticSessionsByWorker[workerFp] ?? {},
  };
}

function installedPipelineSnapshot(
  requestId: string,
  targets: readonly { sessionId: string; viewId: string }[],
) {
  return create(WTerminalPipelineSnapshotSchema, {
    requestId,
    sessions: targets.map((target) => create(TerminalPipelineSessionSnapshotSchema, target)),
  });
}

function installWorker(workerFp: string): void {
  __setConnectWorkerForTest(workerFp, {
    workerFp,
    send(frame) {
      if (frame.frame.case === "browserCommand") {
        const command = JSON.parse(frame.frame.value.frameJson) as Record<string, unknown>;
        expect(command).toEqual({
          kind: "diag-snapshot",
          request_id: frame.frame.value.requestId,
        });
        sentWorkerFps.push(workerFp);
        expect(resolvePendingRpc(
          frame.frame.value.requestId,
          installedWorkerSnapshot(workerFp),
          workerFp,
        )).toBe(true);
        return 1;
      }
      if (frame.frame.case === "terminalPipelineSnapshot") {
        const request = frame.frame.value;
        sentPipelineTargetsByWorker[workerFp] = request.targets.map((target) => ({
          sessionId: target.sessionId,
          viewId: target.viewId,
        }));
        expect(resolvePendingRpc(
          request.requestId,
          installedPipelineSnapshot(request.requestId, request.targets),
          workerFp,
        )).toBe(true);
        return 1;
      }
      throw new Error("unexpected diagnostic worker frame");
    },
  });
}

function returnedSessionIds(snapshot: SnapshotPayload): string[] {
  return Object.values(snapshot.workers).flatMap((worker) =>
    worker.status === "ok" && worker.snapshot
      ? Object.keys(worker.snapshot.sessions)
      : []
  ).sort();
}

function returnedPipelineSessionIds(snapshot: SnapshotPayload): string[] {
  return Object.values(snapshot.workers).flatMap((worker) =>
    worker.terminal_pipeline?.status === "ok" && worker.terminal_pipeline.snapshot
      ? worker.terminal_pipeline.snapshot.sessions.map((session) => session.session_id)
      : []
  ).sort();
}

beforeAll(async () => {
  workdir = mkdtempSync(join(tmpdir(), "roost-diag-snapshot-handlers-"));
  const opened = openDb(join(workdir, "coord.db"));
  db = opened.db;
  closeDb = opened.close;
  await runMigrations(opened.sqlite);
  tenant = ensureSelfHostedTenant(opened.sqlite, { backfillLegacyScopes: false });
  await db.insertInto("workers").values([
    { fp: WORKER_A, dashboard_id: tenant.dashboardId, label: "A", os: "linux", registered_at_ms: 1, last_seen_ms: 1 },
    { fp: WORKER_C, dashboard_id: tenant.dashboardId, label: "C", os: "linux", registered_at_ms: 1, last_seen_ms: 1 },
    { fp: WORKER_LOCAL, dashboard_id: tenant.dashboardId, label: "local", os: "linux", registered_at_ms: 1, last_seen_ms: 1 },
  ]).execute();
  await db.insertInto("sessions").values([
    ...BATCH_SESSION_IDS.map((sessionId, index) => openSession(
      sessionId,
      index < 32 ? WORKER_A : WORKER_C,
      index + 1,
    )),
    openSession(LOCAL_UNSELECTED_SESSION, WORKER_LOCAL, 65),
  ]).execute();
});

beforeEach(() => {
  sentWorkerFps = [];
  sentPipelineTargetsByWorker = {};
  installWorker(WORKER_A);
  installWorker(WORKER_C);
  installWorker(WORKER_LOCAL);
});

afterEach(() => {
  for (const workerFp of [WORKER_A, WORKER_C, WORKER_LOCAL]) {
    rejectPendingRpcsForWorker(workerFp, "test cleanup");
    __setConnectWorkerForTest(workerFp, null);
  }
});

afterAll(async () => {
  await closeDb?.();
  rmSync(workdir, { recursive: true, force: true });
});

describe("DiagSnapshot session filters", () => {
  test("keeps singular session_filter_id compatibility", async () => {
    const handlers = makeSystemHandlers({ db } as unknown as ConnectDeps);
    const sessionId = BATCH_SESSION_IDS[0]!;
    const response = await handlers.diagSnapshot(
      diagRequest({ sessionFilterId: sessionId }),
      deviceContext(),
    );
    const snapshot = snapshotPayload(response);

    expect(Object.keys(snapshot.coord.sessions)).toEqual([sessionId]);
    expect(Object.keys(snapshot.workers)).toEqual([WORKER_A]);
    expect(returnedSessionIds(snapshot)).toEqual([sessionId]);
    expect(sentWorkerFps).toEqual([WORKER_A]);
    expect(returnedPipelineSessionIds(snapshot)).toEqual([sessionId]);
    expect(sentPipelineTargetsByWorker).toEqual({
      [WORKER_A]: [{ sessionId, viewId: "" }],
    });
  });

  test("admits 64 local IDs and targets only their workers", async () => {
    const handlers = makeSystemHandlers({ db } as unknown as ConnectDeps);
    const response = await handlers.diagSnapshot(
      diagRequest({ sessionFilterIds: BATCH_SESSION_IDS }),
      deviceContext(),
    );
    const snapshot = snapshotPayload(response);

    expect(Object.keys(snapshot.coord.sessions).sort()).toEqual([...BATCH_SESSION_IDS].sort());
    expect(Object.keys(snapshot.workers).sort()).toEqual([WORKER_A, WORKER_C]);
    expect(returnedSessionIds(snapshot)).toEqual([...BATCH_SESSION_IDS].sort());
    expect(sentWorkerFps.sort()).toEqual([WORKER_A, WORKER_C]);
    expect(returnedPipelineSessionIds(snapshot)).toEqual([...BATCH_SESSION_IDS].sort());
    expect(Object.keys(sentPipelineTargetsByWorker).sort()).toEqual([WORKER_A, WORKER_C]);
  });

  test("excludes unknown batch IDs and unrelated workers", async () => {
    const handlers = makeSystemHandlers({ db } as unknown as ConnectDeps);
    const localSessionId = BATCH_SESSION_IDS[0]!;
    const response = await handlers.diagSnapshot(
      diagRequest({ sessionFilterIds: [localSessionId, MISSING_SESSION] }),
      deviceContext(),
    );
    const snapshot = snapshotPayload(response);

    expect(Object.keys(snapshot.coord.sessions)).toEqual([localSessionId]);
    expect(Object.keys(snapshot.workers)).toEqual([WORKER_A]);
    expect(returnedSessionIds(snapshot)).toEqual([localSessionId]);
    expect(sentWorkerFps).toEqual([WORKER_A]);
    expect(returnedPipelineSessionIds(snapshot)).toEqual([localSessionId]);
    expect(sentPipelineTargetsByWorker).toEqual({
      [WORKER_A]: [{ sessionId: localSessionId, viewId: "" }],
    });
    expect(snapshot.workers[WORKER_LOCAL]).toBeUndefined();
  });

  test("rejects oversized and ambiguous filter input before dispatch", async () => {
    const handlers = makeSystemHandlers({ db } as unknown as ConnectDeps);
    const sessionId = BATCH_SESSION_IDS[0]!;
    for (const request of [
      diagRequest({ sessionFilterIds: [...BATCH_SESSION_IDS, LOCAL_UNSELECTED_SESSION] }),
      diagRequest({ sessionFilterId: sessionId, sessionFilterIds: [sessionId] }),
      diagRequest({ sessionFilterIds: [sessionId, sessionId] }),
      diagRequest({ sessionFilterIds: [""] }),
    ]) {
      await expect(handlers.diagSnapshot(request, deviceContext()))
        .rejects.toMatchObject({ code: Code.InvalidArgument });
    }
    expect(sentWorkerFps).toEqual([]);
    expect(sentPipelineTargetsByWorker).toEqual({});
  });

  test("includes every open session and routable worker unfiltered", async () => {
    const handlers = makeSystemHandlers({ db } as unknown as ConnectDeps);
    const response = await handlers.diagSnapshot(diagRequest(), deviceContext());
    const snapshot = snapshotPayload(response);

    expect(Object.keys(snapshot.coord.sessions).sort()).toEqual([
      ...BATCH_SESSION_IDS,
      LOCAL_UNSELECTED_SESSION,
    ].sort());
    expect(Object.keys(snapshot.workers).sort()).toEqual([WORKER_A, WORKER_C, WORKER_LOCAL]);
    expect(sentWorkerFps.sort()).toEqual([WORKER_A, WORKER_C, WORKER_LOCAL]);
    expect(returnedPipelineSessionIds(snapshot)).toEqual([
      ...BATCH_SESSION_IDS,
      LOCAL_UNSELECTED_SESSION,
    ].sort());
  });
});
