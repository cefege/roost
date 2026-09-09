// DiagSnapshot handler coverage for normalized session filters, dashboard
// isolation, and worker fan-out. A migrated database provides the durable
// session/worker scope while fake connections resolve real pending RPCs.

import { create } from "@bufbuild/protobuf";
import {
  Code,
  createContextValues,
  type HandlerContext,
} from "@connectrpc/connect";
import { afterAll, afterEach, beforeAll, beforeEach, describe, expect, test } from "bun:test";
import { DiagSnapshotRequestSchema } from "@roost/shared/proto/coordinator_pb";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import {
  callerKey,
  dashboardActorKey,
  type DashboardActor,
} from "../src/connect/auth-interceptor.ts";
import { makeSystemHandlers } from "../src/connect/handlers-system.ts";
import { __setConnectWorkerForTest } from "../src/connect/worker-registry.ts";
import type { ConnectDeps } from "../src/connect/router.ts";
import { rejectPendingRpcsForWorker, resolvePendingRpc } from "../src/router/pending-rpcs.ts";
import { openDb, type KyselyDB } from "../src/db/connection.ts";
import { runMigrations } from "../src/db/migrate.ts";

const ORGANIZATION_ID = "diag-snapshot-organization";
const DASHBOARD_A = "diag-snapshot-dashboard-a";
const DASHBOARD_B = "diag-snapshot-dashboard-b";
const WORKER_A = "a1b2c3d4".repeat(8);
const WORKER_C = "c3d4e5f6".repeat(8);
const WORKER_LOCAL = "d4e5f6a7".repeat(8);
const WORKER_FOREIGN = "b2c3d4e5".repeat(8);
const LOCAL_UNSELECTED_SESSION = "91000000-0000-4000-8000-000000000065";
const FOREIGN_SESSION = "91000000-0000-4000-8000-000000000066";
const BATCH_SESSION_IDS = Array.from(
  { length: 64 },
  (_, index) => `91000000-0000-4000-8000-${String(index + 1).padStart(12, "0")}`,
);

const ADMIN_ACTOR: DashboardActor = {
  accountId: "diag-snapshot-account",
  organizationId: ORGANIZATION_ID,
  dashboardId: DASHBOARD_A,
  organizationRole: "owner",
  dashboardRole: "admin",
  deviceFingerprint: "diag-snapshot-admin-device",
};
const MEMBER_ACTOR: DashboardActor = {
  ...ADMIN_ACTOR,
  dashboardRole: "member",
  deviceFingerprint: "diag-snapshot-member-device",
};

type SnapshotPayload = {
  coord: { sessions: Record<string, unknown> };
  workers: Record<string, {
    status: string;
    snapshot?: { sessions: Record<string, unknown> };
  }>;
};

let workdir = "";
let db: KyselyDB;
let closeDb: () => Promise<void>;
let sentWorkerFps: string[] = [];

const diagnosticSessionsByWorker: Record<string, Record<string, unknown>> = {
  [WORKER_A]: Object.fromEntries(BATCH_SESSION_IDS.slice(0, 32).map((sessionId) => [sessionId, {}])),
  [WORKER_C]: Object.fromEntries(BATCH_SESSION_IDS.slice(32).map((sessionId) => [sessionId, {}])),
  [WORKER_LOCAL]: { [LOCAL_UNSELECTED_SESSION]: {} },
  [WORKER_FOREIGN]: { [FOREIGN_SESSION]: {} },
};

function actorContext(actor: DashboardActor): HandlerContext {
  const values = createContextValues();
  values.set(callerKey, {
    kind: "account-device",
    fingerprint: actor.deviceFingerprint,
    label: "diagnostic test device",
    accountId: actor.accountId,
  });
  values.set(dashboardActorKey, actor);
  return { values, signal: new AbortController().signal } as unknown as HandlerContext;
}

function diagRequest(overrides: Partial<{
  sessionFilterId: string;
  sessionFilterIds: string[];
}> = {}) {
  return create(DiagSnapshotRequestSchema, overrides);
}

function openSession(id: string, dashboardId: string, workerFp: string, channel: number) {
  return {
    id,
    dashboard_id: dashboardId,
    worker_fp: workerFp,
    channel,
    kind: "shell" as const,
    cwd: "/tmp",
    status: "open" as const,
    created_at: 1,
  };
}

function snapshotPayload(response: { snapshotJson: string }): SnapshotPayload {
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

function installWorker(workerFp: string, dashboardId: string): void {
  __setConnectWorkerForTest(workerFp, {
    workerFp,
    dashboardId,
    send(frame) {
      expect(frame.frame.case).toBe("browserCommand");
      if (frame.frame.case !== "browserCommand") throw new Error("expected diagnostic browser command");
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

beforeAll(async () => {
  workdir = mkdtempSync(join(tmpdir(), "roost-diag-snapshot-handlers-"));
  const opened = openDb(join(workdir, "coord.db"));
  db = opened.db;
  closeDb = opened.close;
  await runMigrations(opened.sqlite);
  await db.insertInto("organizations").values({
    id: ORGANIZATION_ID,
    slug: "diag-snapshot",
    name: "Diagnostic snapshots",
    status: "active",
    created_at_ms: 1,
  }).execute();
  await db.insertInto("dashboards").values([
    { id: DASHBOARD_A, organization_id: ORGANIZATION_ID, slug: "diag-a", name: "A", status: "active", created_at_ms: 1 },
    { id: DASHBOARD_B, organization_id: ORGANIZATION_ID, slug: "diag-b", name: "B", status: "active", created_at_ms: 1 },
  ]).execute();
  await db.insertInto("workers").values([
    { fp: WORKER_A, dashboard_id: DASHBOARD_A, label: "A", os: "linux", registered_at_ms: 1, last_seen_ms: 1 },
    { fp: WORKER_C, dashboard_id: DASHBOARD_A, label: "C", os: "linux", registered_at_ms: 1, last_seen_ms: 1 },
    { fp: WORKER_LOCAL, dashboard_id: DASHBOARD_A, label: "local", os: "linux", registered_at_ms: 1, last_seen_ms: 1 },
    { fp: WORKER_FOREIGN, dashboard_id: DASHBOARD_B, label: "foreign", os: "linux", registered_at_ms: 1, last_seen_ms: 1 },
  ]).execute();
  await db.insertInto("sessions").values([
    ...BATCH_SESSION_IDS.map((sessionId, index) => openSession(
      sessionId,
      DASHBOARD_A,
      index < 32 ? WORKER_A : WORKER_C,
      index + 1,
    )),
    openSession(LOCAL_UNSELECTED_SESSION, DASHBOARD_A, WORKER_LOCAL, 65),
    openSession(FOREIGN_SESSION, DASHBOARD_B, WORKER_FOREIGN, 66),
  ]).execute();
});

beforeEach(() => {
  sentWorkerFps = [];
  installWorker(WORKER_A, DASHBOARD_A);
  installWorker(WORKER_C, DASHBOARD_A);
  installWorker(WORKER_LOCAL, DASHBOARD_A);
  installWorker(WORKER_FOREIGN, DASHBOARD_B);
});

afterEach(() => {
  for (const workerFp of [WORKER_A, WORKER_C, WORKER_LOCAL, WORKER_FOREIGN]) {
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
      actorContext(MEMBER_ACTOR),
    );
    const snapshot = snapshotPayload(response);

    expect(Object.keys(snapshot.coord.sessions)).toEqual([sessionId]);
    expect(Object.keys(snapshot.workers)).toEqual([WORKER_A]);
    expect(returnedSessionIds(snapshot)).toEqual([sessionId]);
    expect(sentWorkerFps).toEqual([WORKER_A]);
  });

  test("admits 64 local IDs and targets only their workers", async () => {
    const handlers = makeSystemHandlers({ db } as unknown as ConnectDeps);
    const response = await handlers.diagSnapshot(
      diagRequest({ sessionFilterIds: BATCH_SESSION_IDS }),
      actorContext(MEMBER_ACTOR),
    );
    const snapshot = snapshotPayload(response);

    expect(Object.keys(snapshot.coord.sessions).sort()).toEqual([...BATCH_SESSION_IDS].sort());
    expect(Object.keys(snapshot.workers).sort()).toEqual([WORKER_A, WORKER_C]);
    expect(returnedSessionIds(snapshot)).toEqual([...BATCH_SESSION_IDS].sort());
    expect(sentWorkerFps.sort()).toEqual([WORKER_A, WORKER_C]);
  });

  test("excludes foreign batch IDs and their workers", async () => {
    const handlers = makeSystemHandlers({ db } as unknown as ConnectDeps);
    const localSessionId = BATCH_SESSION_IDS[0]!;
    const response = await handlers.diagSnapshot(
      diagRequest({ sessionFilterIds: [localSessionId, FOREIGN_SESSION] }),
      actorContext(MEMBER_ACTOR),
    );
    const snapshot = snapshotPayload(response);

    expect(Object.keys(snapshot.coord.sessions)).toEqual([localSessionId]);
    expect(Object.keys(snapshot.workers)).toEqual([WORKER_A]);
    expect(returnedSessionIds(snapshot)).toEqual([localSessionId]);
    expect(sentWorkerFps).toEqual([WORKER_A]);
    expect(snapshot.workers[WORKER_FOREIGN]).toBeUndefined();
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
      await expect(handlers.diagSnapshot(request, actorContext(MEMBER_ACTOR)))
        .rejects.toMatchObject({ code: Code.InvalidArgument });
    }
    expect(sentWorkerFps).toEqual([]);
  });

  test("reserves unfiltered snapshots for dashboard admins", async () => {
    const handlers = makeSystemHandlers({ db } as unknown as ConnectDeps);
    await expect(handlers.diagSnapshot(diagRequest(), actorContext(MEMBER_ACTOR)))
      .rejects.toMatchObject({ code: Code.PermissionDenied });
    expect(sentWorkerFps).toEqual([]);

    const response = await handlers.diagSnapshot(diagRequest(), actorContext(ADMIN_ACTOR));
    const snapshot = snapshotPayload(response);

    expect(Object.keys(snapshot.coord.sessions).sort()).toEqual([
      ...BATCH_SESSION_IDS,
      LOCAL_UNSELECTED_SESSION,
    ].sort());
    expect(Object.keys(snapshot.workers).sort()).toEqual([WORKER_A, WORKER_C, WORKER_LOCAL]);
    expect(sentWorkerFps.sort()).toEqual([WORKER_A, WORKER_C, WORKER_LOCAL]);
    expect(snapshot.workers[WORKER_FOREIGN]).toBeUndefined();
  });
});
