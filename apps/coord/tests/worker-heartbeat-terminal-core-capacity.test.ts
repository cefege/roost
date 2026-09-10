// Pins coordinator persistence for worker-owned terminal-core admission reports.
// A malformed or omitted report clears stale capacity rather than advertising an
// allocation limit the currently running worker did not prove.

import { expect, test } from "bun:test";
import { create } from "@bufbuild/protobuf";
import { Code, ConnectError } from "@connectrpc/connect";
import { terminalCoreCapacityReportToProto } from "@roost/shared/terminal-core-capacity-proto";
import { WorkersHeartbeatRequestSchema } from "@roost/shared/proto/coordinator_pb";
import { TerminalCoreCapacityReportSchema } from "@roost/shared/proto/wire_pb";
import { presenceBus } from "../src/buses.ts";
import {
  workerHeartbeatContext,
  workerHeartbeatDb,
  workerHeartbeatHandlers,
} from "./worker-heartbeat-fixture.ts";

const DASHBOARD_ID = "worker-terminal-core-capacity-dashboard";
const CAPACITY_REPORT = {
  used: 12,
  pending: 0,
  capacity: 12,
  estimated_reserved_bytes: 480 * 1024 * 1024,
  effective_memory_ceiling_bytes: 2 * 1024 * 1024 * 1024,
  boot_rss_bytes: 256 * 1024 * 1024,
  overcommit_count: 0,
  refusal_count: 3,
} as const;

function capacityPresenceReports(): {
  seen: Array<{
    kind: "heartbeat" | "registered";
    report: typeof CAPACITY_REPORT | null;
  }>;
  stop: () => void;
} {
  const seen: Array<{
    kind: "heartbeat" | "registered";
    report: typeof CAPACITY_REPORT | null;
  }> = [];
  const stop = presenceBus.subscribe((message) => {
    if (message.kind === "registered") {
      seen.push({
        kind: "registered",
        report: message.worker.terminal_core_capacity as typeof CAPACITY_REPORT | null,
      });
    } else if (message.kind === "heartbeat") {
      seen.push({
        kind: "heartbeat",
        report: message.terminal_core_capacity as typeof CAPACITY_REPORT | null,
      });
    }
  });
  return { seen, stop };
}

test("a valid terminal-core capacity report persists and projects", async () => {
  const database = workerHeartbeatDb(DASHBOARD_ID);
  const handlers = workerHeartbeatHandlers(database.db);
  const presence = capacityPresenceReports();

  try {
    await handlers.workersHeartbeat(
      create(WorkersHeartbeatRequestSchema, {
        terminalCoreCapacity: terminalCoreCapacityReportToProto(CAPACITY_REPORT),
      }),
      workerHeartbeatContext(),
    );
  } finally {
    presence.stop();
  }

  expect(JSON.parse(String(database.row().terminal_core_capacity_json)))
    .toEqual(CAPACITY_REPORT);
  expect(presence.seen).toEqual([{
    kind: "heartbeat",
    report: CAPACITY_REPORT,
  }]);
});

test("a malformed terminal-core capacity report clears stale capacity", async () => {
  const database = workerHeartbeatDb(DASHBOARD_ID, {
    terminal_core_capacity_json: JSON.stringify(CAPACITY_REPORT),
  });
  const handlers = workerHeartbeatHandlers(database.db);
  const presence = capacityPresenceReports();

  let rejection: unknown;
  try {
    await handlers.workersHeartbeat(
      create(WorkersHeartbeatRequestSchema, {
        terminalCoreCapacity: create(TerminalCoreCapacityReportSchema, {
          ...terminalCoreCapacityReportToProto(CAPACITY_REPORT),
          used: 13,
        }),
      }),
      workerHeartbeatContext(),
    );
  } catch (error) {
    rejection = error;
  } finally {
    presence.stop();
  }

  expect(rejection).toBeInstanceOf(ConnectError);
  if (rejection instanceof ConnectError) {
    expect(rejection.code).toBe(Code.InvalidArgument);
    expect(rejection.rawMessage).toBe("terminal core capacity report is malformed");
  }
  expect(database.row().terminal_core_capacity_json).toBeNull();
  expect(presence.seen).toEqual([{
    kind: "heartbeat",
    report: null,
  }]);
});

test("an omitted terminal-core capacity report retires stale capacity", async () => {
  const database = workerHeartbeatDb(DASHBOARD_ID, {
    terminal_core_capacity_json: JSON.stringify(CAPACITY_REPORT),
  });
  const handlers = workerHeartbeatHandlers(database.db);

  await handlers.workersHeartbeat(
    create(WorkersHeartbeatRequestSchema, {}),
    workerHeartbeatContext(),
  );

  expect(database.patches[0]).toMatchObject({ terminal_core_capacity_json: null });
  expect(database.row().terminal_core_capacity_json).toBeNull();
});
