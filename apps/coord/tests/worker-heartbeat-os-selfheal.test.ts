// Pins the heartbeat's platform self-heal: workers.os is otherwise written only
// at bootstrap redeem and boot register, so a row could keep naming a machine
// that no longer holds the key fingerprint. Every beat re-asserts the beating
// process's platform, and a flip is logged as worker_os_changed.

import { expect, test } from "bun:test";
import { create } from "@bufbuild/protobuf";
import { Code, ConnectError } from "@connectrpc/connect";
import { WorkersHeartbeatRequestSchema } from "@roost/shared/proto/coordinator_pb";
import { presenceBus } from "../src/buses.ts";
import {
  WORKER_LABEL,
  workerHeartbeatContext,
  workerHeartbeatDb,
  workerHeartbeatHandlers,
} from "./worker-heartbeat-fixture.ts";

const DASHBOARD_ID = "worker-os-selfheal-dashboard";

test("a heartbeat re-points a row at the platform actually beating on it", async () => {
  const database = workerHeartbeatDb(DASHBOARD_ID, { os: "linux" });
  const handlers = workerHeartbeatHandlers(database.db);
  const registered: Array<{ os: string; label: string }> = [];
  const stop = presenceBus.subscribe((msg) => {
    if (msg.kind === "registered") {
      registered.push({ os: msg.worker.os, label: msg.worker.label });
    }
  }, DASHBOARD_ID);

  try {
    await handlers.workersHeartbeat(
      create(WorkersHeartbeatRequestSchema, { os: "darwin" }),
      workerHeartbeatContext(DASHBOARD_ID),
    );
  } finally {
    stop();
  }

  expect(database.patches[0]).toMatchObject({ os: "darwin" });
  expect(database.row().os).toBe("darwin");
  expect(registered).toEqual([{ os: "darwin", label: WORKER_LABEL }]);
});

test("a heartbeat without a platform keeps the stored one", async () => {
  const database = workerHeartbeatDb(DASHBOARD_ID, { os: "darwin" });
  const handlers = workerHeartbeatHandlers(database.db);

  await handlers.workersHeartbeat(
    create(WorkersHeartbeatRequestSchema, {}),
    workerHeartbeatContext(DASHBOARD_ID),
  );

  expect(database.patches[0]).not.toHaveProperty("os");
  expect(database.row().os).toBe("darwin");
});

test("a heartbeat carrying an unknown platform is rejected and writes nothing", async () => {
  const database = workerHeartbeatDb(DASHBOARD_ID, { os: "darwin" });
  const handlers = workerHeartbeatHandlers(database.db);

  let rejection: unknown;
  try {
    await handlers.workersHeartbeat(
      create(WorkersHeartbeatRequestSchema, { os: "plan9" }),
      workerHeartbeatContext(DASHBOARD_ID),
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
