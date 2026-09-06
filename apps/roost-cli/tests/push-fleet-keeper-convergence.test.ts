// Fleet convergence follows each direct deploy's exact action-time proof.
// Global checks retain stable keeper identity where possible but do not compare
// dynamic bindings or source identities with the older preflight snapshot.

import { describe, expect, test } from "bun:test";
import { KEEPER_EMPTY_BINDING_DIGEST } from "@roost/shared/keeper-update";
import { _atomicFleetConvergenceProblems } from "../src/push.ts";
import type { FleetRolloutWorker } from "../src/push-fleet-rollout.ts";
import type { WorkerStatus } from "../src/status.ts";

const PRIOR_SHA = "a".repeat(40);
const TARGET_SHA = "b".repeat(40);
const KEEPER_DIGEST = "c".repeat(64);
const KEEPER_EPOCH = "00000000-0000-4000-8000-000000000002";

function keeperContract(buildSha: string) {
  return {
    protocol_version: 2,
    supported_features: ["keeper-contract-v1"],
    required_features: ["keeper-contract-v1"],
    implementation_digest: KEEPER_DIGEST,
    bun_abi: "1.2.3",
    platform: "linux" as const,
    arch: "x64",
    build_sha: buildSha,
  };
}

const target: FleetRolloutWorker = {
  fingerprint: "1".repeat(64),
  host: "alpha.example",
  keeperUpdate: {
    admission: {
      classification: "worker-only-safe",
      source_contract_digest: KEEPER_DIGEST,
      target_contract_digest: KEEPER_DIGEST,
      expected_keeper_pid: 41,
      expected_keeper_epoch: KEEPER_EPOCH,
      expected_binding_digest: KEEPER_EMPTY_BINDING_DIGEST,
      required_action: "preserve",
    },
    source_contract: keeperContract(PRIOR_SHA),
    target_contract: keeperContract(TARGET_SHA),
  },
};

function status(overrides: Partial<WorkerStatus> = {}): WorkerStatus {
  return {
    fingerprint: target.fingerprint,
    label: "alpha",
    os: "linux",
    reachableAddr: target.host,
    gitSha: PRIOR_SHA,
    coordinatorOpenSessionIds: [],
    keeperRuntime: {
      schema_version: 1,
      running_contract: keeperContract(PRIOR_SHA),
      keeper_pid: 41,
      keeper_epoch: KEEPER_EPOCH,
      channel_count: 0,
      binding_digest: KEEPER_EMPTY_BINDING_DIGEST,
      reconciled_at_ms: 1,
    },
    lastSeenMs: 10,
    ageMs: 0,
    stale: false,
    ...overrides,
  };
}

describe("fleet action-time keeper convergence", () => {
  test("accepts source identity established by the direct rollback", () => {
    const rolledBack = status({
      coordinatorOpenSessionIds: ["00000000-0000-4000-8000-000000000003"],
      keeperRuntime: {
        schema_version: 1,
        running_contract: keeperContract(PRIOR_SHA),
        keeper_pid: 99,
        keeper_epoch: "00000000-0000-4000-8000-000000000004",
        channel_count: 1,
        binding_digest: "e".repeat(64),
        reconciled_at_ms: 2,
      },
    });
    expect(_atomicFleetConvergenceProblems(
      [rolledBack], [target], PRIOR_SHA, "rollback", new Map(),
    )).toEqual([]);
    expect(_atomicFleetConvergenceProblems(
      [{ ...rolledBack, gitSha: TARGET_SHA }],
      [target], TARGET_SHA, "finalize", new Map(),
    )).toContain("alpha: preserved keeper identity changed");
  });

  test("accepts target bindings established by the direct rollout", () => {
    const finalized = status({
      gitSha: TARGET_SHA,
      coordinatorOpenSessionIds: ["00000000-0000-4000-8000-000000000003"],
      keeperRuntime: {
        schema_version: 1,
        running_contract: keeperContract(TARGET_SHA),
        keeper_pid: 41,
        keeper_epoch: KEEPER_EPOCH,
        channel_count: 1,
        binding_digest: "e".repeat(64),
        reconciled_at_ms: 2,
      },
    });
    expect(_atomicFleetConvergenceProblems(
      [finalized], [target], TARGET_SHA, "finalize", new Map(),
    )).toEqual([]);
  });
});

test("rejects a fresh heartbeat from a disconnected worker", () => {
  expect(_atomicFleetConvergenceProblems(
    [status({ gitSha: TARGET_SHA })],
    [target],
    TARGET_SHA,
    "finalize",
    new Map(),
    0,
    new Set(),
  )).toContain("alpha: worker is not coordinator-routable");
});
