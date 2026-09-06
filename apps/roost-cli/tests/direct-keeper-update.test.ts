// Direct keeper callback tests pin action-time preserve identity across rollback.
// The worker response, not a stale staging observation, becomes the restart
// convergence fence while coordinator and CLI clocks may be arbitrarily skewed.

import { expect, test } from "bun:test";
import type { JournaledKeeperUpdateV1 } from "@roost/shared/keeper-update";
import {
  createJournaledKeeperUpdateCallbacks,
  localUpdateWorker,
  localUpdateWorkerForAdmission,
} from "../src/direct-keeper-update.ts";
import type { WorkerStatus } from "../src/status.ts";

const WORKER_FINGERPRINT = "a".repeat(64);
const SOURCE_SHA = "b".repeat(40);
const IMPLEMENTATION_DIGEST = "c".repeat(64);
const STAGED_EPOCH = "10000000-0000-4000-8000-000000000001";
const ACTION_EPOCH = "20000000-0000-4000-8000-000000000002";
const STAGED_BINDING = "d".repeat(64);
const ACTION_BINDING = "e".repeat(64);

const contract = {
  protocol_version: 1,
  supported_features: ["keeper-contract-v1"],
  required_features: ["keeper-contract-v1"],
  implementation_digest: IMPLEMENTATION_DIGEST,
  bun_abi: "test",
  platform: "linux" as const,
  arch: "x64",
  build_sha: SOURCE_SHA,
};

const update: JournaledKeeperUpdateV1 = {
  admission: {
    classification: "worker-only-safe",
    source_contract_digest: IMPLEMENTATION_DIGEST,
    target_contract_digest: IMPLEMENTATION_DIGEST,
    expected_keeper_pid: 41,
    expected_keeper_epoch: STAGED_EPOCH,
    expected_binding_digest: STAGED_BINDING,
    required_action: "preserve",
  },
  source_contract: contract,
  target_contract: { ...contract, build_sha: "f".repeat(40) },
};

function worker(reconciledAtMs: number, lastSeenMs: number): WorkerStatus {
  return {
    fingerprint: WORKER_FINGERPRINT,
    label: "worker",
    os: "linux",
    reachableAddr: null,
    gitSha: SOURCE_SHA,
    keeperRuntime: {
      schema_version: 1,
      running_contract: contract,
      keeper_pid: 99,
      keeper_epoch: ACTION_EPOCH,
      channel_count: 1,
      binding_digest: ACTION_BINDING,
      reconciled_at_ms: reconciledAtMs,
    },
    coordinatorOpenSessionIds: ["30000000-0000-4000-8000-000000000003"],
    lastSeenMs,
    ageMs: 0,
    stale: false,
  };
}

test("source preserve proof uses the authenticated action-time keeper identity", async () => {
  let inventory = [worker(10, 20)];
  const callbacks = createJournaledKeeperUpdateCallbacks({
    attempts: 1,
    sleep: async () => {},
    routable: async () => true,
    inventory: () => inventory,
    prepare: async () => ({
      outcome: "preserved",
      keeperPid: 99n,
      keeperEpoch: ACTION_EPOCH,
      bindingDigest: ACTION_BINDING,
    }),
  });

  await callbacks.apply(WORKER_FINGERPRINT, update, "source");
  inventory = [worker(11, 21)];
  await expect(callbacks.prove(
    WORKER_FINGERPRINT,
    update,
    "source",
    SOURCE_SHA,
    10_000,
  )).resolves.toBeUndefined();
});

test("local resolution uses reachable addresses instead of worker labels", async () => {
  const local = worker(10, 20);
  local.label = "custom";
  local.reachableAddr = "local-address";
  const remote = { ...worker(10, 20), fingerprint: "f".repeat(64) };
  remote.label = "roost-host";
  remote.reachableAddr = "remote-address";

  await expect(localUpdateWorker(
    [local, remote],
    async host => host === "local-address",
  )).resolves.toBe(local);
});

test("local first-install admission permits an empty inventory only for bootstrap", async () => {
  await expect(localUpdateWorkerForAdmission(
    true,
    [],
    async () => false,
  )).resolves.toBeNull();
  await expect(localUpdateWorkerForAdmission(
    false,
    [],
    async () => false,
  )).rejects.toThrow("exactly one local worker");
});
