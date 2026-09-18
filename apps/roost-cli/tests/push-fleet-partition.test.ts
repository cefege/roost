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
  const contract = {
    build_sha: "b".repeat(40),
    bun_abi: "1.3.14",
    platform: "linux",
    arch: "x64",
    entry_digest: "d".repeat(64),
    protocol_version: 1,
  };

  test("a keeper that cannot be adopted defers that machine and admits the others", () => {
    // The live incident this pins: one machine whose keeper holds PTYs the new
    // release cannot adopt used to refuse the whole push with zero mutation.
    const result = classifyFleetKeeperUpdates(
      [
        { fingerprint: ALPHA_FP, host: "alpha.example" },
        { fingerprint: BETA_FP, host: "beta.example" },
      ],
      [
        status({ fingerprint: ALPHA_FP, label: "alpha", keeperRuntime: null }),
        status({ fingerprint: BETA_FP, label: "beta", keeperRuntime: null }),
      ],
      new Map([[ALPHA_FP, contract]]),
    );
    expect(result.workers).toEqual([]);
    expect(result.deferred.map((machine) => `${machine.label}: ${machine.reason}`)).toEqual([
      "alpha: keeper update admission is unproven",
      "beta: target keeper runtime proof is unavailable",
    ]);
  });
});
