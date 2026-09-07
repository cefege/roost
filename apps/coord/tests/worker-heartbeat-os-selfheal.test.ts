// Pins the heartbeat's platform self-heal: workers.os is otherwise written only
// at bootstrap redeem and boot register, so a row could keep naming a machine
// that no longer holds the key fingerprint. Every beat re-asserts the beating
// process's platform, and a flip is logged as worker_os_changed.

import { expect, test } from "bun:test";
import { create } from "@bufbuild/protobuf";
import { Code, ConnectError, type HandlerContext } from "@connectrpc/connect";
import { WorkersHeartbeatRequestSchema } from "@roost/shared/proto/coordinator_pb";
import { callerKey } from "../src/connect/auth-interceptor.ts";
import { makeWorkerHandlers } from "../src/connect/handlers-workers.ts";
import type { ConnectDeps } from "../src/connect/router.ts";
import { presenceBus } from "../src/buses.ts";

const WORKER_FP = "c".repeat(64);
const DASHBOARD_ID = "worker-os-selfheal-dashboard";

type WorkerRow = Record<string, unknown>;

function workerDb(initialOs: string): {
  db: ConnectDeps["db"];
  row: () => WorkerRow;
  patches: WorkerRow[];
} {
  let row: WorkerRow = {
    fp: WORKER_FP,
    dashboard_id: DASHBOARD_ID,
    label: "mac old",
    os: initialOs,
    git_sha: null,
    host_metrics_json: null,
    registered_at_ms: 1,
    last_seen_ms: 1,
    reachable_addr: null,
    keeper_runtime_json: null,
  };
  const patches: WorkerRow[] = [];
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
        let patch: WorkerRow = {};
        const updateQuery = {
          set: (value: WorkerRow) => {
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
    row: () => row,
    patches,
  };
}

function workerContext(): HandlerContext {
  const worker = {
    kind: "worker" as const,
    fingerprint: WORKER_FP,
    label: "mac old",
    dashboardId: DASHBOARD_ID,
  };
  return {
    values: { get: (key: unknown) => key === callerKey ? worker : undefined },
  } as unknown as HandlerContext;
}

function handlersFor(db: ConnectDeps["db"]) {
  return makeWorkerHandlers({
    db,
    cfg: { saasMode: false },
  } as unknown as ConnectDeps);
}

test("a heartbeat re-points a row at the platform actually beating on it", async () => {
  const database = workerDb("linux");
  const handlers = handlersFor(database.db);
  const registered: Array<{ os: string; label: string }> = [];
  const stop = presenceBus.subscribe((msg) => {
    if (msg.kind === "registered") {
      registered.push({ os: msg.worker.os, label: msg.worker.label });
    }
  }, DASHBOARD_ID);

  try {
    await handlers.workersHeartbeat(
      create(WorkersHeartbeatRequestSchema, { os: "darwin" }),
      workerContext(),
    );
  } finally {
    stop();
  }

  expect(database.patches[0]).toMatchObject({ os: "darwin" });
  expect(database.row().os).toBe("darwin");
  expect(registered).toEqual([{ os: "darwin", label: "mac old" }]);
});

test("a heartbeat without a platform keeps the stored one", async () => {
  const database = workerDb("darwin");
  const handlers = handlersFor(database.db);

  await handlers.workersHeartbeat(
    create(WorkersHeartbeatRequestSchema, {}),
    workerContext(),
  );

  expect(database.patches[0]).not.toHaveProperty("os");
  expect(database.row().os).toBe("darwin");
});

test("a heartbeat carrying an unknown platform is rejected and writes nothing", async () => {
  const database = workerDb("darwin");
  const handlers = handlersFor(database.db);

  let rejection: unknown;
  try {
    await handlers.workersHeartbeat(
      create(WorkersHeartbeatRequestSchema, { os: "plan9" }),
      workerContext(),
    );
  } catch (error) {
    rejection = error;
  }

  expect(rejection).toBeInstanceOf(ConnectError);
  if (rejection instanceof ConnectError) {
    expect(rejection.code).toBe(Code.InvalidArgument);
    expect(rejection.rawMessage).toBe("unsupported worker os");
  }
  expect(database.patches).toHaveLength(0);
  expect(database.row().os).toBe("darwin");
});
