import { expect, test } from "bun:test";
import { create } from "@bufbuild/protobuf";
import {
  WorkersHeartbeatRequestSchema,
  WorkersRegisterRequestSchema,
} from "@roost/shared/proto/coordinator_pb";
import { HostIdentitySchema } from "@roost/shared/proto/wire_pb";
import { presenceBus } from "../src/buses.ts";
import {
  workerHeartbeatContext,
  workerHeartbeatDb,
  workerHeartbeatHandlers,
} from "./worker-heartbeat-fixture.ts";

const DASHBOARD_ID = "worker-host-identity-dashboard";
const MAC_IDENTITY = {
  hardware_model: "MacBookPro18,3",
  chip: "Apple M1 Pro",
  linux_distribution: null,
} as const;

const MAC_IDENTITY_PROTO = create(HostIdentitySchema, {
  hardwareModel: MAC_IDENTITY.hardware_model,
  chip: MAC_IDENTITY.chip,
});

test("registration persists static host identity and publishes a full Worker", async () => {
  const database = workerHeartbeatDb(DASHBOARD_ID);
  const handlers = workerHeartbeatHandlers(database.db);
  const published: unknown[] = [];
  const stop = presenceBus.subscribe((message) => {
    if (message.kind === "registered") published.push(message.worker.host_identity);
  });

  try {
    await handlers.workersRegister(
      create(WorkersRegisterRequestSchema, { hostIdentity: MAC_IDENTITY_PROTO }),
      workerHeartbeatContext(),
    );
  } finally {
    stop();
  }

  expect(JSON.parse(String(database.row().host_identity_json))).toEqual(MAC_IDENTITY);
  expect(published).toEqual([MAC_IDENTITY]);
});

test("a changed heartbeat identity publishes a full Worker instead of a heartbeat delta", async () => {
  const database = workerHeartbeatDb(DASHBOARD_ID, {
    host_identity_json: JSON.stringify({
      hardware_model: "MacBookAir10,1",
      chip: "Apple M1",
      linux_distribution: null,
    }),
  });
  const handlers = workerHeartbeatHandlers(database.db);
  const published: string[] = [];
  const stop = presenceBus.subscribe((message) => published.push(message.kind));

  try {
    await handlers.workersHeartbeat(
      create(WorkersHeartbeatRequestSchema, { hostIdentity: MAC_IDENTITY_PROTO }),
      workerHeartbeatContext(),
    );
  } finally {
    stop();
  }

  expect(JSON.parse(String(database.row().host_identity_json))).toEqual(MAC_IDENTITY);
  expect(published).toEqual(["registered"]);
});

test("an old heartbeat without host identity leaves an existing record intact", async () => {
  const database = workerHeartbeatDb(DASHBOARD_ID, {
    host_identity_json: JSON.stringify(MAC_IDENTITY),
  });
  const handlers = workerHeartbeatHandlers(database.db);

  await handlers.workersHeartbeat(
    create(WorkersHeartbeatRequestSchema, {}),
    workerHeartbeatContext(),
  );

  expect(database.row().host_identity_json).toBe(JSON.stringify(MAC_IDENTITY));
  expect(database.patches[0]).not.toHaveProperty("host_identity_json");
});
