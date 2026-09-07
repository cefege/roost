// Direct keeper admission tests pin which registry states can be proven, which
// are unprovable, and which stay refused. The proven path additionally pins
// action-time preserve identity across rollback: the worker response, not a
// stale staging observation, becomes the restart convergence fence while
// coordinator and CLI clocks may be arbitrarily skewed.

import { expect, test } from "bun:test";
import type { JournaledKeeperUpdateV1 } from "@roost/shared/keeper-update";
import {
  createJournaledKeeperUpdateCallbacks,
  directKeeperUpdateAdmission,
  localUpdateWorker,
  localUpdateWorkerForAdmission,
} from "../src/direct-keeper-update.ts";
import { keeperAdmissionStaging } from "../src/keeper-admission-staging.ts";
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

test("a proven worker journals the admitted update from its reported runtime", () => {
  const target = { ...contract, build_sha: "f".repeat(40) };
  const resolved = directKeeperUpdateAdmission(
    WORKER_FINGERPRINT,
    target,
    false,
    [worker(10, 20)],
  );

  expect(keeperAdmissionStaging("mac-host", "macOS", resolved)).toEqual({
    keeperUpdate: {
      admission: {
        classification: "worker-only-safe",
        source_contract_digest: IMPLEMENTATION_DIGEST,
        target_contract_digest: IMPLEMENTATION_DIGEST,
        expected_keeper_pid: 99,
        expected_keeper_epoch: ACTION_EPOCH,
        expected_binding_digest: ACTION_BINDING,
        required_action: "preserve",
      },
      source_contract: contract,
      target_contract: target,
    },
    workerFingerprint: WORKER_FINGERPRINT,
    installedServiceRefusal: null,
  });
});

test("a worker that never reported a keeper runtime stages without a journal", () => {
  const unreported: WorkerStatus = { ...worker(10, 20), keeperRuntime: null };
  const resolved = directKeeperUpdateAdmission(
    WORKER_FINGERPRINT,
    contract,
    false,
    [unreported],
  );

  expect(resolved).toEqual({ outcome: "runtime-unreported", workerLabel: "worker" });
  expect(keeperAdmissionStaging("mac-host", "macOS", resolved)).toEqual({
    keeperUpdate: null,
    workerFingerprint: null,
    installedServiceRefusal: null,
  });
});

test("a reported keeper runtime that refuses the update stays refused", () => {
  expect(() => directKeeperUpdateAdmission(
    WORKER_FINGERPRINT,
    { ...contract, implementation_digest: "9".repeat(64) },
    true,
    [worker(10, 20)],
  )).toThrow("keeper update is blocked or unproven");
});

test("a stale registry row defers to host evidence instead of failing admission", () => {
  const resolved = directKeeperUpdateAdmission(
    WORKER_FINGERPRINT,
    contract,
    false,
    [{ ...worker(10, 20), stale: true }],
  );

  expect(resolved).toEqual({ outcome: "proof-stale", workerLabel: "worker" });
  const staging = keeperAdmissionStaging("mac-host", "macOS", resolved);
  expect(staging.keeperUpdate).toBeNull();
  expect(staging.workerFingerprint).toBeNull();
  expect(staging.installedServiceRefusal).toContain("stale keeper update proof");
});

test("a stale local row blocks keeper maintenance but not deploy admission", async () => {
  const stale: WorkerStatus = {
    ...worker(10, 20),
    reachableAddr: "local-address",
    stale: true,
  };
  const isLocal = async (host: string): Promise<boolean> => host === "local-address";

  await expect(localUpdateWorker([stale], isLocal))
    .rejects.toThrow("self-update keeper runtime proof is stale");
  await expect(localUpdateWorkerForAdmission(false, [stale], isLocal))
    .resolves.toBe(stale);
});

test("an unregistered host is an outcome only when bootstrap is authorized", () => {
  expect(directKeeperUpdateAdmission("unknown-host", contract, true, []))
    .toEqual({ outcome: "unregistered" });
  expect(() => directKeeperUpdateAdmission("unknown-host", contract, false, []))
    .toThrow("cannot resolve exactly one worker");
});
