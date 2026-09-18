// Rollout partition and identity-integrity proofs for `roost push`.
// One sleeping machine must never wedge the fleet, so these pin which
// registered workers a rollout converges now, which ones it defers and with
// which reason, and which registry defects still refuse the whole push.

import { describe, expect, test } from "bun:test";
import {
  _deferredFleetReportLines,
  _partitionFleetForRollout,
  fleetWorkerIdentityProblems,
} from "../src/push.ts";
import { classifyFleetKeeperUpdates } from "../src/push-keeper-admission.ts";
import {
  KEEPER_EMPTY_BINDING_DIGEST,
  KeeperRuntimeObservationV1Schema,
  type KeeperContractV1,
  type KeeperRuntimeObservationV1,
} from "@roost/shared/keeper-update";
import type { WorkerStatus } from "../src/status.ts";

const PRIOR_SHA = "a".repeat(40);
const DRIFT_SHA = "c".repeat(40);
const ALPHA_FP = "1".repeat(64);
const BETA_FP = "2".repeat(64);
const GAMMA_FP = "3".repeat(64);

function status(overrides: Partial<WorkerStatus> = {}): WorkerStatus {
  return {
    fingerprint: ALPHA_FP,
    label: "alpha",
    os: "linux",
    reachableAddr: "alpha.example",
    gitSha: PRIOR_SHA,
    keeperRuntime: null,
    coordinatorOpenSessionIds: [],
    lastSeenMs: 10,
    ageMs: 0,
    stale: false,
    ...overrides,
  };
}

const candidates = [
  { fingerprint: ALPHA_FP, host: "alpha.example" },
  { fingerprint: BETA_FP, host: "beta.example" },
  { fingerprint: GAMMA_FP, host: "gamma.example" },
];

describe("fleet rollout partition", () => {
  test("converges the reachable prior-SHA workers and defers the rest with a reason", () => {
    const partition = _partitionFleetForRollout(
      candidates,
      [
        status(),
        status({ fingerprint: BETA_FP, label: "beta", reachableAddr: "beta.example" }),
        status({
          fingerprint: GAMMA_FP,
          label: "gamma",
          reachableAddr: "gamma.example",
          ageMs: 120_000,
          stale: true,
        }),
      ],
      new Set([ALPHA_FP, GAMMA_FP]),
      PRIOR_SHA,
    );
    expect(partition.participants).toEqual([
      { fingerprint: ALPHA_FP, host: "alpha.example" },
    ]);
    expect(partition.deferred).toEqual([
      { fingerprint: BETA_FP, label: "beta", reason: "not reachable" },
      { fingerprint: GAMMA_FP, label: "gamma", reason: "stale" },
    ]);
  });

  test("defers a machine that came back on another commit, naming both SHAs", () => {
    const partition = _partitionFleetForRollout(
      [candidates[0]!],
      [status({ gitSha: DRIFT_SHA })],
      new Set([ALPHA_FP]),
      PRIOR_SHA,
    );
    expect(partition.participants).toEqual([]);
    expect(partition.deferred).toEqual([{
      fingerprint: ALPHA_FP,
      label: "alpha",
      reason: "reports cccccccc, prior is aaaaaaaa",
    }]);
  });

  test("a fleet with nobody reachable on the prior SHA leaves zero participants", () => {
    const partition = _partitionFleetForRollout(
      candidates,
      [
        status({ stale: true }),
        status({ fingerprint: BETA_FP, label: "beta", gitSha: null }),
      ],
      new Set([ALPHA_FP, BETA_FP]),
      PRIOR_SHA,
    );
    expect(partition.participants).toEqual([]);
    expect(partition.deferred.map(
      (machine) => `${machine.label}: ${machine.reason}`,
    )).toEqual([
      "alpha: stale",
      "beta: reports no SHA, prior is aaaaaaaa",
      "gamma.example: no longer registered",
    ]);
  });
});

describe("fleet worker identity integrity", () => {
  test("refuses a duplicate identity or a malformed fingerprint", () => {
    expect(fleetWorkerIdentityProblems([
      status(),
      status(),
      status({ fingerprint: "nope", label: "bent" }),
    ])).toEqual([
      `${ALPHA_FP}: duplicate coordinator worker identity`,
      "bent: invalid worker fingerprint",
    ]);
  });

  test("version skew and keeper gaps are not identity defects", () => {
    expect(fleetWorkerIdentityProblems([
      status({ gitSha: DRIFT_SHA, stale: true, keeperRuntime: null }),
    ])).toEqual([]);
  });
});

describe("deferred machine report", () => {
  test("names every deferred machine, its reason, and both ways it catches up", () => {
    const lines = _deferredFleetReportLines([
      { fingerprint: ALPHA_FP, label: "m1-us", reason: "not reachable" },
      { fingerprint: BETA_FP, label: "m2-eu", reason: "reports cccccccc, prior is aaaaaaaa" },
    ]);
    expect(lines[0]).toContain("2 machines deferred");
    const report = lines.join("\n");
    expect(report).toContain("m1-us: not reachable");
    expect(report).toContain("m2-eu: reports cccccccc, prior is aaaaaaaa");
    expect(report).toContain("automatically when it next attaches to the coordinator");
    expect(report).toContain("roost deploy <host>");
    expect(_deferredFleetReportLines([
      { fingerprint: ALPHA_FP, label: "m1-us", reason: "stale" },
    ])[0]).toContain("1 machine deferred");
  });

  test("a wholly reachable fleet prints no deferral block", () => {
    expect(_deferredFleetReportLines([])).toEqual([]);
  });
});

describe("keeper admission deferral", () => {
  const targetContract: KeeperContractV1 = {
    protocol_version: 1,
    supported_features: [],
    required_features: [],
    implementation_digest: "d".repeat(64),
    bun_abi: "1.3.14",
    platform: "linux",
    arch: "x64",
    build_sha: "b".repeat(40),
  };
  // A DIFFERENT implementation: this is what makes the release unable to adopt
  // the running keeper, so its live sessions decide the outcome.
  const otherImplementation: KeeperContractV1 = {
    ...targetContract,
    implementation_digest: "e".repeat(64),
  };

  function runtime(
    runningContract: KeeperContractV1,
    channelCount: number,
  ): KeeperRuntimeObservationV1 {
    return KeeperRuntimeObservationV1Schema.parse({
      schema_version: 1,
      running_contract: runningContract,
      keeper_pid: 4242,
      keeper_epoch: "0f1e2d3c-4b5a-4987-8765-432109abcdef",
      channel_count: channelCount,
      binding_digest: channelCount === 0
        ? KEEPER_EMPTY_BINDING_DIGEST
        : "a".repeat(64),
      reconciled_at_ms: 1_700_000_000_000,
    });
  }

  test("a keeper holding live sessions is deferred while the adoptable machine is admitted", () => {
    // The live incident this pins: mike-m5-air's keeper held 44 PTYs the new
    // release could not adopt, which used to refuse the WHOLE push with zero
    // mutation and so blocked every other machine too.
    const result = classifyFleetKeeperUpdates(
      [
        { fingerprint: ALPHA_FP, host: "alpha.example" },
        { fingerprint: BETA_FP, host: "beta.example" },
      ],
      [
        status({
          fingerprint: ALPHA_FP,
          label: "alpha",
          keeperRuntime: runtime(otherImplementation, 2),
          coordinatorOpenSessionIds: ["s-1", "s-2"],
        }),
        status({
          fingerprint: BETA_FP,
          label: "beta",
          keeperRuntime: runtime(targetContract, 1),
          coordinatorOpenSessionIds: ["s-3"],
        }),
      ],
      new Map([[ALPHA_FP, targetContract], [BETA_FP, targetContract]]),
    );

    expect(result.deferred.map((machine) => machine.label)).toEqual(["alpha"]);
    expect(result.deferred[0]!.reason).toContain("keeper cannot be adopted");
    expect(result.deferred[0]!.reason).toContain("roost keeper-refresh");
    expect(result.workers.map((worker) => worker.fingerprint)).toEqual([BETA_FP]);
    expect(result.workers[0]!.keeperUpdate.admission.required_action).toBe("preserve");
  });

  test("a machine whose target keeper contract could not be probed is deferred, not fatal", () => {
    const result = classifyFleetKeeperUpdates(
      [{ fingerprint: ALPHA_FP, host: "alpha.example" }],
      [status({ fingerprint: ALPHA_FP, label: "alpha", keeperRuntime: null })],
      new Map(),
    );
    expect(result.workers).toEqual([]);
    expect(result.deferred.map((machine) => `${machine.label}: ${machine.reason}`)).toEqual([
      "alpha: target keeper runtime proof is unavailable",
    ]);
  });
});
