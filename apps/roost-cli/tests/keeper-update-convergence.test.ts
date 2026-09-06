// Keeper convergence tests separate coordinator receipt time from the worker's
// clock. Direct rollout proof requires a fresh heartbeat and a reconciliation
// value different from the source observation.

import { expect, test } from "bun:test";
import { keeperUpdateConvergenceProblem } from "../src/keeper-update-convergence.ts";
import {
  LOCAL_KEEPER_UPDATE,
  WORKER_FINGERPRINT,
} from "./deploy-local-journal-fixture.ts";

test("rejects a fresh heartbeat carrying stale keeper reconciliation", () => {
  const problem = keeperUpdateConvergenceProblem({
    fingerprint: WORKER_FINGERPRINT,
    label: "local",
    os: "linux",
    reachableAddr: null,
    gitSha: "b".repeat(40),
    coordinatorOpenSessionIds: [],
    lastSeenMs: 101,
    ageMs: 0,
    stale: false,
    keeperRuntime: {
      schema_version: 1,
      running_contract: LOCAL_KEEPER_UPDATE.target_contract,
      keeper_pid: 900,
      keeper_epoch: "44444444-4444-4444-8444-444444444444",
      channel_count: 0,
      binding_digest: LOCAL_KEEPER_UPDATE.admission.expected_binding_digest,
      reconciled_at_ms: 100,
    },
  }, LOCAL_KEEPER_UPDATE, "target", 100, 100);
  expect(problem).toBe("local: awaiting a post-rollout keeper reconciliation");
});

test("accepts a changed reconciliation value across skewed host clocks", () => {
  const problem = keeperUpdateConvergenceProblem({
    fingerprint: WORKER_FINGERPRINT,
    label: "local",
    os: "linux",
    reachableAddr: null,
    gitSha: "b".repeat(40),
    coordinatorOpenSessionIds: [],
    lastSeenMs: 101,
    ageMs: 0,
    stale: false,
    keeperRuntime: {
      schema_version: 1,
      running_contract: LOCAL_KEEPER_UPDATE.target_contract,
      keeper_pid: 900,
      keeper_epoch: "44444444-4444-4444-8444-444444444444",
      channel_count: 0,
      binding_digest: LOCAL_KEEPER_UPDATE.admission.expected_binding_digest,
      reconciled_at_ms: 50,
    },
  }, LOCAL_KEEPER_UPDATE, "target", 100, 1_000);
  expect(problem).toBeNull();
});

test("rejects a preserved keeper with a different admitted binding digest", () => {
  const preserveUpdate = {
    ...LOCAL_KEEPER_UPDATE,
    admission: {
      ...LOCAL_KEEPER_UPDATE.admission,
      classification: "worker-only-safe" as const,
      source_contract_digest:
        LOCAL_KEEPER_UPDATE.source_contract.implementation_digest!,
      target_contract_digest:
        LOCAL_KEEPER_UPDATE.source_contract.implementation_digest!,
      expected_keeper_pid: 77,
      expected_keeper_epoch: "55555555-5555-4555-8555-555555555555",
      expected_binding_digest: "e".repeat(64),
      required_action: "preserve" as const,
    },
    target_contract: {
      ...LOCAL_KEEPER_UPDATE.source_contract,
      build_sha: "b".repeat(40),
    },
  };
  const problem = keeperUpdateConvergenceProblem({
    fingerprint: WORKER_FINGERPRINT,
    label: "local",
    os: "linux",
    reachableAddr: null,
    gitSha: "b".repeat(40),
    coordinatorOpenSessionIds: ["00000000-0000-4000-8000-000000000001"],
    lastSeenMs: 101,
    ageMs: 0,
    stale: false,
    keeperRuntime: {
      schema_version: 1,
      running_contract: preserveUpdate.target_contract,
      keeper_pid: 77,
      keeper_epoch: "55555555-5555-4555-8555-555555555555",
      channel_count: 1,
      binding_digest: "f".repeat(64),
      reconciled_at_ms: 101,
    },
  }, preserveUpdate, "target");
  expect(problem).toBe("local: preserved keeper bindings changed");
});
