import { expect, test } from "bun:test";
import { create } from "@bufbuild/protobuf";
import { Code, type HandlerContext } from "@connectrpc/connect";
import {
  WorkersHeartbeatRequestSchema,
  WorkersHeartbeatResponseSchema,
  WorkersRegisterRequestSchema,
  WorkersRenameRequestSchema,
} from "@roost/shared/proto/coordinator_pb";
import {
  callerKey,
  dashboardActorKey,
} from "../src/connect/auth-interceptor.ts";
import { makeWorkerHandlers } from "../src/connect/handlers-workers.ts";
import type { ConnectDeps } from "../src/connect/router.ts";

const WORKER_FP = "a".repeat(64);
const DASHBOARD_ID = "worker-bounds-dashboard";

type WorkerPatch = Record<string, unknown>;

function recordingWorkerDb(): {
  db: ConnectDeps["db"];
  patches: WorkerPatch[];
} {
  let row: WorkerPatch = {
    fp: WORKER_FP,
    dashboard_id: DASHBOARD_ID,
    label: "worker",
    os: "linux",
    git_sha: null,
    host_metrics_json: null,
    registered_at_ms: 1,
    last_seen_ms: 1,
    reachable_addr: null,
    keeper_stale: null,
  };
  const patches: WorkerPatch[] = [];
  const selectQuery = {
    select: () => selectQuery,
    selectAll: () => selectQuery,
    where: () => selectQuery,
    executeTakeFirst: async () => row,
  };

  return {
    db: {
      selectFrom: () => selectQuery,
      updateTable: () => {
        let patch: WorkerPatch = {};
        const updateQuery = {
          set: (value: WorkerPatch) => {
            patch = value;
            return updateQuery;
          },
          where: () => updateQuery,
          returningAll: () => updateQuery,
          executeTakeFirst: async () => {
            row = { ...row, ...patch };
            patches.push(patch);
            return row;
          },
          executeTakeFirstOrThrow: async () => {
            row = { ...row, ...patch };
            patches.push(patch);
            return row;
          },
        };
        return updateQuery;
      },
    } as unknown as ConnectDeps["db"],
    patches,
  };
}

function workerContext(): HandlerContext {
  const worker = {
    kind: "worker" as const,
    fingerprint: WORKER_FP,
    label: "worker",
    dashboardId: DASHBOARD_ID,
  };
  return {
    values: { get: (key: unknown) => key === callerKey ? worker : undefined },
  } as unknown as HandlerContext;
}

function adminContext(): HandlerContext {
  const caller = {
    kind: "account-device" as const,
    fingerprint: "admin-device",
    label: "Admin",
    accountId: "account",
  };
  const actor = {
    accountId: caller.accountId,
    organizationId: "organization",
    dashboardId: DASHBOARD_ID,
    organizationRole: "owner" as const,
    dashboardRole: "admin" as const,
    deviceFingerprint: caller.fingerprint,
  };
  return {
    values: {
      get: (key: unknown) => {
        if (key === callerKey) return caller;
        if (key === dashboardActorKey) return actor;
        return undefined;
      },
    },
  } as unknown as HandlerContext;
}

test("worker register, heartbeat, and rename cap every persisted string at a UTF-8 boundary", async () => {
  const database = recordingWorkerDb();
  const handlers = makeWorkerHandlers({
    db: database.db,
    cfg: { saasMode: false },
  } as unknown as ConnectDeps);

  await handlers.workersRegister(create(WorkersRegisterRequestSchema, {
    label: `${"😀".repeat(1025)}x`,
    gitSha: "g".repeat(4097),
    reachableAddr: `${"r".repeat(4095)}😀`,
  }), workerContext());
  expect(database.patches[0]).toMatchObject({
    label: "😀".repeat(1024),
    git_sha: "g".repeat(4096),
    reachable_addr: "r".repeat(4095),
  });

  const heartbeat = await handlers.workersHeartbeat(create(WorkersHeartbeatRequestSchema, {
    gitSha: "é".repeat(3000),
    keeperStale: "k".repeat(5000),
    reachableAddr: `${"a".repeat(4094)}éz`,
  }), workerContext());
  expect(heartbeat).toEqual(create(WorkersHeartbeatResponseSchema, {}));
  expect(database.patches[1]).toMatchObject({
    git_sha: "é".repeat(2048),
    keeper_stale: "k".repeat(4096),
    reachable_addr: `${"a".repeat(4094)}é`,
  });

  await handlers.workersRename(create(WorkersRenameRequestSchema, {
    fp: WORKER_FP,
    label: `${"b".repeat(4095)}é`,
  }), adminContext());
  expect(database.patches[2]).toMatchObject({ label: "b".repeat(4095) });

  for (const patch of database.patches) {
    for (const value of Object.values(patch)) {
      if (typeof value === "string") {
        expect(Buffer.byteLength(value, "utf8")).toBeLessThanOrEqual(4096);
      }
    }
  }
});

test("browser principals cannot register or heartbeat as workers", async () => {
  const handlers = makeWorkerHandlers({
    cfg: { saasMode: true },
  } as unknown as ConnectDeps);
  await expect(handlers.workersRegister(
    create(WorkersRegisterRequestSchema, {}),
    adminContext(),
  )).rejects.toMatchObject({ code: Code.Unauthenticated });
  await expect(handlers.workersHeartbeat(
    create(WorkersHeartbeatRequestSchema, {}),
    adminContext(),
  )).rejects.toMatchObject({ code: Code.Unauthenticated });
});
