// Pins the coordinator half of keeper-runtime reporting: a heartbeat's proof
// lands in workers.keeper_runtime_json intact enough for update admission to
// read it back, a malformed proof clears the column instead of persisting a
// half-parsed one, and a beat that omits the proof retires the stored one.

import { expect, test } from "bun:test";
import { create } from "@bufbuild/protobuf";
import { Code, ConnectError } from "@connectrpc/connect";
import {
  KeeperRuntimeObservationV1Schema,
  keeperUpdateAdmission,
  type KeeperContractV1,
  type KeeperRuntimeObservationV1,
} from "@roost/shared/keeper-update";
import { keeperRuntimeObservationToProto } from "@roost/shared/keeper-update-proto";
import { WorkersHeartbeatRequestSchema } from "@roost/shared/proto/coordinator_pb";
import { KeeperRuntimeObservationV1Schema as KeeperRuntimeProtoSchema } from "@roost/shared/proto/wire_pb";
import { presenceBus } from "../src/buses.ts";
import {
  workerHeartbeatContext,
  workerHeartbeatDb,
  workerHeartbeatHandlers,
} from "./worker-heartbeat-fixture.ts";

const DASHBOARD_ID = "worker-keeper-runtime-dashboard";
const OPEN_SESSION_IDS = [
  "00000000-0000-4000-8000-000000000001",
  "00000000-0000-4000-8000-000000000002",
];

const RUNNING_CONTRACT: KeeperContractV1 = {
  protocol_version: 3,
  supported_features: ["history-records", "spawn-epoch"],
  required_features: ["history-records"],
  implementation_digest: "b".repeat(64),
  bun_abi: "1.3.14",
  platform: "darwin",
  arch: "arm64",
  build_sha: "c".repeat(40),
};

const OBSERVATION: KeeperRuntimeObservationV1 = {
  schema_version: 1,
  running_contract: RUNNING_CONTRACT,
  keeper_pid: 4242,
  keeper_epoch: "3f2504e0-4f89-41d3-9a0c-0305e82c3301",
  channel_count: OPEN_SESSION_IDS.length,
  binding_digest: "a".repeat(64),
  reconciled_at_ms: 1_700_000_000_000,
};

function registeredKeeperRuntimes(): {
  seen: Array<KeeperRuntimeObservationV1 | null>;
  stop: () => void;
} {
  const seen: Array<KeeperRuntimeObservationV1 | null> = [];
  const stop = presenceBus.subscribe((msg) => {
    if (msg.kind === "registered") seen.push(msg.worker.keeper_runtime);
  }, DASHBOARD_ID);
  return { seen, stop };
}

test("a proved keeper runtime persists in a shape update admission accepts", async () => {
  const database = workerHeartbeatDb(DASHBOARD_ID);
  const handlers = workerHeartbeatHandlers(database.db);
  const presence = registeredKeeperRuntimes();

  try {
    await handlers.workersHeartbeat(
      create(WorkersHeartbeatRequestSchema, {
        keeperRuntime: keeperRuntimeObservationToProto(OBSERVATION),
      }),
      workerHeartbeatContext(DASHBOARD_ID),
    );
  } finally {
    presence.stop();
  }

  const stored = database.row().keeper_runtime_json;
  expect(typeof stored).toBe("string");
  const readBack = KeeperRuntimeObservationV1Schema.parse(
    JSON.parse(String(stored)),
  );
  expect(readBack).toEqual(OBSERVATION);
  expect(presence.seen).toEqual([OBSERVATION]);

  // The deploy gate reads this row and refuses to update a worker whose keeper
  // proof it cannot classify, so persistence is only worth anything if the
  // stored value still admits an update.
  expect(keeperUpdateAdmission(
    RUNNING_CONTRACT,
    readBack,
    new Set(OPEN_SESSION_IDS),
  )).toMatchObject({
    classification: "worker-only-safe",
    expected_keeper_pid: OBSERVATION.keeper_pid,
    expected_keeper_epoch: OBSERVATION.keeper_epoch,
    expected_binding_digest: OBSERVATION.binding_digest,
    required_action: "preserve",
  });
});

test("a malformed keeper runtime clears the column and rejects the beat", async () => {
  const database = workerHeartbeatDb(DASHBOARD_ID, {
    keeper_runtime_json: JSON.stringify(OBSERVATION),
  });
  const handlers = workerHeartbeatHandlers(database.db);
  const presence = registeredKeeperRuntimes();

  let rejection: unknown;
  try {
    await handlers.workersHeartbeat(
      create(WorkersHeartbeatRequestSchema, {
        keeperRuntime: create(KeeperRuntimeProtoSchema, { schemaVersion: 1 }),
      }),
      workerHeartbeatContext(DASHBOARD_ID),
    );
  } catch (error) {
    rejection = error;
  } finally {
    presence.stop();
  }

  expect(rejection).toBeInstanceOf(ConnectError);
  if (rejection instanceof ConnectError) {
    expect(rejection.code).toBe(Code.InvalidArgument);
    expect(rejection.rawMessage).toBe("keeper runtime observation is malformed");
  }
  expect(database.row().keeper_runtime_json).toBeNull();
  expect(presence.seen).toEqual([null]);
});

test("a beat that omits the keeper runtime retires the stored proof", async () => {
  const database = workerHeartbeatDb(DASHBOARD_ID, {
    keeper_runtime_json: JSON.stringify(OBSERVATION),
  });
  const handlers = workerHeartbeatHandlers(database.db);
  const presence = registeredKeeperRuntimes();

  try {
    await handlers.workersHeartbeat(
      create(WorkersHeartbeatRequestSchema, {}),
      workerHeartbeatContext(DASHBOARD_ID),
    );
  } finally {
    presence.stop();
  }

  expect(database.patches[0]).toMatchObject({ keeper_runtime_json: null });
  expect(database.row().keeper_runtime_json).toBeNull();
  expect(presence.seen).toEqual([null]);
});
