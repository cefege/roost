import { expect, test } from "bun:test";
import {
  workerRowToProto,
  workerRowToWirePresence,
} from "../src/wire/row-proto.ts";

const LEGACY_WORKER_ROW = {
  fp: "a".repeat(64),
  label: "legacy worker",
  os: "linux",
  git_sha: null,
  host_metrics_json: null,
  registered_at_ms: 1,
  last_seen_ms: 2,
  reachable_addr: null,
};

test("worker rows created before host identity decode to a null wire identity", () => {
  expect(workerRowToWirePresence(LEGACY_WORKER_ROW).host_identity).toBeNull();
  expect(workerRowToProto(LEGACY_WORKER_ROW).hostIdentity).toBeUndefined();
});

test("worker list and registered presence retain static host identity", () => {
  const row = {
    ...LEGACY_WORKER_ROW,
    host_identity_json: JSON.stringify({
      hardware_model: "MacBookAir10,1",
      chip: "Apple M1",
      linux_distribution: null,
    }),
  };

  expect(workerRowToWirePresence(row).host_identity).toEqual({
    hardware_model: "MacBookAir10,1",
    chip: "Apple M1",
    linux_distribution: null,
  });
  expect(workerRowToProto(row).hostIdentity).toMatchObject({
    hardwareModel: "MacBookAir10,1",
    chip: "Apple M1",
  });
});
