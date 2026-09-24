// Atomic fleet rollout regression tests exercise the durable decision boundary.
// They use an in-memory coordinator/worker driver so ordering and exhaustive
// rollback behavior remain visible without mutating host services.

import { describe, expect, test } from "bun:test";
import { mkdirSync, mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { KEEPER_EMPTY_BINDING_DIGEST } from "@roost/protocol/keeper-update";
import {
  convergeAtomicFleet,
  interruptedFleetRecoveryAction,
  finishAtomicFleetFinalization,
  type AtomicFleetRolloutDeps,
  type FleetRolloutPlan,
} from "../src/push-fleet-rollout.ts";
import {
  _atomicFleetConvergenceProblems,
  _resolveAtomicFleetWorkers,
  push,
} from "../src/push.ts";
import { sameRolloutTarget } from "../src/push-fleet-plan.ts";
import {
  _handleCoordinatorInitialJournalWriteFailure,
  acquireFleetPushTransaction,
} from "../src/push-coordinator.ts";
import type { CoordinatorDeployJournalV2 } from "../src/coordinator-deploy-journal.ts";
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

function keeperUpdate() {
  return {
    admission: {
      classification: "worker-only-safe" as const,
      source_contract_digest: KEEPER_DIGEST,
      target_contract_digest: KEEPER_DIGEST,
      expected_keeper_pid: 41,
      expected_keeper_epoch: KEEPER_EPOCH,
      expected_binding_digest: KEEPER_EMPTY_BINDING_DIGEST,
      required_action: "preserve" as const,
    },
    source_contract: keeperContract(PRIOR_SHA),
    target_contract: keeperContract(TARGET_SHA),
  };
}
const plan: FleetRolloutPlan = {
  rolloutId: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
  admissionRecordedAtMs: 1,
  priorSha: PRIOR_SHA,
  targetSha: TARGET_SHA,
  workers: [
    { fingerprint: "1".repeat(64), host: "alpha.example", keeperUpdate: keeperUpdate() },
    { fingerprint: "2".repeat(64), host: "beta.example", keeperUpdate: keeperUpdate() },
  ],
};

function rolloutDriver(options: {
  fail?: string;
  targetProblems?: string[];
  rollbackProblems?: string[];
  beginFailure?: boolean;
  decisionStillConverging?: boolean;
} = {}): { events: string[]; deps: AtomicFleetRolloutDeps } {
  const events: string[] = [];
  return {
    events,
    deps: {
      deployWorker: async (worker, directive) => {
        const event = `${directive.action}:${worker.host}`;
        events.push(event);
        if (options.fail === event) throw new Error("injected worker failure");
      },
      proveFleet: async (sha, action) => {
        events.push(`prove:${action}:${sha}`);
        return action === "rollback"
          ? (options.rollbackProblems ?? [])
          : (options.targetProblems ?? []);
      },
      beginCoordinatorFinalization: async () => {
        events.push("coordinator:begin-finalization");
        if (options.beginFailure) throw new Error("ambiguous finalization checkpoint");
      },
      coordinatorCanRollback: async () => options.decisionStillConverging === true,
      finalizeCoordinator: async (finishWorkers) => {
        events.push("coordinator:finalizing");
        await finishWorkers();
        events.push("coordinator:finalized");
      },
      rollbackCoordinator: async (rollbackWorkers) => {
        events.push("coordinator:rollback-start");
        await rollbackWorkers();
        events.push("coordinator:rollback-done");
      },
    },
  };
}

function status(overrides: Partial<WorkerStatus> = {}): WorkerStatus {
  return {
    fingerprint: "1".repeat(64),
    label: "alpha",
    os: "linux",
    reachableAddr: "alpha.example",
    gitSha: PRIOR_SHA,
    keeperRuntime: {
      schema_version: 1,
      running_contract: keeperContract(PRIOR_SHA),
      keeper_pid: 41,
      keeper_epoch: KEEPER_EPOCH,
      channel_count: 0,
      binding_digest: KEEPER_EMPTY_BINDING_DIGEST,
      reconciled_at_ms: 1,
    },
    coordinatorOpenSessionIds: [],
    lastSeenMs: 10,
    ageMs: 0,
    stale: false,
    ...overrides,
  };
}

describe("atomic fleet decision", () => {
  test("finalizes workers only after target convergence and durable decision", async () => {
    const { events, deps } = rolloutDriver();
    await convergeAtomicFleet(plan, deps);
    expect(events).toEqual([
      "hold:alpha.example",
      "hold:beta.example",
      `prove:hold:${TARGET_SHA}`,
      "coordinator:begin-finalization",
      "coordinator:finalizing",
      "finalize:alpha.example",
      "finalize:beta.example",
      `prove:finalize:${TARGET_SHA}`,
      "coordinator:finalized",
    ]);
  });

  test("an ambiguous commit checkpoint leaves every participant held", async () => {
    const { events, deps } = rolloutDriver({ beginFailure: true });
    await expect(convergeAtomicFleet(plan, deps))
      .rejects.toThrow("ambiguous finalization checkpoint");
    expect(events).toContain("coordinator:begin-finalization");
    expect(events.some((event) => event.startsWith("finalize:"))).toBeFalse();
    expect(events.some((event) => event.startsWith("rollback:"))).toBeFalse();
  });

  test("a proven pre-decision failure performs full rollback", async () => {
    const { events, deps } = rolloutDriver({
      beginFailure: true,
      decisionStillConverging: true,
    });
    await expect(convergeAtomicFleet(plan, deps))
      .rejects.toThrow("prior worker and keeper convergence was re-proven");
    expect(events).toContain("rollback:alpha.example");
    expect(events).toContain("rollback:beta.example");
  });

  test("interrupted convergence resumes only the same exact target", () => {
    expect(interruptedFleetRecoveryAction("fleet-converging", true)).toBe("converge-target");
    expect(interruptedFleetRecoveryAction("fleet-converging", false)).toBe("rollback-fleet");
    expect(interruptedFleetRecoveryAction("finalizing", false)).toBe("finish-target");
    expect(interruptedFleetRecoveryAction("activating", true)).toBe("coordinator-rollback");
  });

  test("one worker failure dispatches rollback to every target before coordinator rollback", async () => {
    const { events, deps } = rolloutDriver({ fail: "hold:beta.example" });
    await expect(convergeAtomicFleet(plan, deps)).rejects.toThrow("prior worker and keeper convergence was re-proven");
    expect(events).toContain("hold:alpha.example");
    expect(events).toContain("hold:beta.example");
    expect(events).toContain("rollback:alpha.example");
    expect(events).toContain("rollback:beta.example");
    expect(events.indexOf("coordinator:rollback-start"))
      .toBeLessThan(events.indexOf("rollback:alpha.example"));
    expect(events.indexOf("rollback:beta.example"))
      .toBeLessThan(events.indexOf("coordinator:rollback-done"));
    expect(events).not.toContain("coordinator:begin-finalization");
  });

  test("failed exact convergence rolls every worker back instead of committing a subset", async () => {
    const { events, deps } = rolloutDriver({ targetProblems: ["beta: stale"] });
    await expect(convergeAtomicFleet(plan, deps)).rejects.toThrow("prior worker and keeper convergence was re-proven");
    expect(events).toContain("rollback:alpha.example");
    expect(events).toContain("rollback:beta.example");
    expect(events).not.toContain("coordinator:begin-finalization");
  });

  test("a finalization failure never crosses back to rollback", async () => {
    const { events, deps } = rolloutDriver({ fail: "finalize:beta.example" });
    await expect(convergeAtomicFleet(plan, deps)).rejects.toThrow("fleet finalize did not settle");
    expect(events).toContain("coordinator:begin-finalization");
    expect(events.some((event) => event.startsWith("rollback:"))).toBeFalse();
    expect(events).not.toContain("coordinator:rollback-start");
  });

  test("interrupted finalizing recovery only replays idempotent finalization", async () => {
    const { events, deps } = rolloutDriver();
    await finishAtomicFleetFinalization(plan, deps);
    expect(events).toEqual([
      "coordinator:finalizing",
      "finalize:alpha.example",
      "finalize:beta.example",
      `prove:finalize:${TARGET_SHA}`,
      "coordinator:finalized",
    ]);
  });

  test("an incomplete worker rollback prevents coordinator rollback completion", async () => {
    const { events, deps } = rolloutDriver({
      fail: "rollback:beta.example",
      targetProblems: ["beta: stale"],
    });
    await expect(convergeAtomicFleet(plan, deps)).rejects.toThrow("full rollback is incomplete");
    expect(events).not.toContain("coordinator:rollback-done");
  });
});

describe("fleet journal participant proof", () => {
  test("later registrations do not strand rollback or durable finalization", () => {
    const later = status({
      fingerprint: "2".repeat(64),
      label: "later",
      reachableAddr: "later.example",
    });
    const journaled = [plan.workers[0]!];
    const rollback = (worker: WorkerStatus) => _atomicFleetConvergenceProblems(
      [status(), worker],
      journaled,
      PRIOR_SHA,
      "rollback",
      new Map(),
      0,
      null,
      TARGET_SHA,
    );
    expect(rollback(later)).toEqual([]);
    expect(rollback({ ...later, gitSha: TARGET_SHA })).toEqual([]);
    expect(rollback({ ...later, stale: true })).toEqual([]);
    expect(rollback({ ...later, gitSha: "c".repeat(40) })).toContain(
      `later: unjournaled worker reports ${"c".repeat(8)}, outside this rollout`,
    );
    const atTarget = [
      { ...status(), gitSha: TARGET_SHA },
      { ...later, gitSha: TARGET_SHA },
    ];
    expect(_atomicFleetConvergenceProblems(
      atTarget, journaled, TARGET_SHA, "finalize", new Map(),
    )).toEqual([]);
    expect(_atomicFleetConvergenceProblems(
      atTarget, journaled, TARGET_SHA, "hold", new Map(),
    )).toEqual([]);
    expect(_atomicFleetConvergenceProblems(
      [{ ...later, gitSha: TARGET_SHA }], journaled, TARGET_SHA, "hold", new Map(),
    )).toContain(
      `${plan.workers[0]!.fingerprint}: missing from coordinator worker inventory`,
    );
  });

  test("uses coordinator heartbeat baselines instead of CLI wall-clock time", () => {
    const targetWorker = { ...status(), gitSha: TARGET_SHA, lastSeenMs: 101 };
    expect(_atomicFleetConvergenceProblems(
      [targetWorker],
      [plan.workers[0]!],
      TARGET_SHA,
      "finalize",
      new Map([[targetWorker.fingerprint, 100]]),
      10_000,
    )).toEqual([]);
    expect(_atomicFleetConvergenceProblems(
      [{ ...targetWorker, lastSeenMs: 100 }],
      [plan.workers[0]!],
      TARGET_SHA,
      "finalize",
      new Map([[targetWorker.fingerprint, 100]]),
      0,
    )).toContain("alpha: awaiting a post-rollout heartbeat");
  });

  test("an interrupted rollout resumes the same commit beside a deferred machine", () => {
    const candidates = [
      { fingerprint: plan.workers[0]!.fingerprint, host: "alpha.example" },
      { fingerprint: plan.workers[1]!.fingerprint, host: "beta.example" },
      { fingerprint: "3".repeat(64), host: "later.example" },
    ];
    expect(sameRolloutTarget(plan, TARGET_SHA, candidates)).toBeTrue();
    expect(sameRolloutTarget(plan, TARGET_SHA, candidates.slice(0, 1))).toBeFalse();
    expect(sameRolloutTarget(plan, PRIOR_SHA, candidates)).toBeFalse();
  });

});

describe("fleet push admission", () => {

  test("rejects non-atomic flags before reading repository or fleet state", async () => {
    await expect(push(["--allow-dirty"])).rejects.toThrow("never permits --allow-dirty");
    await expect(push(["--no-coord"])).rejects.toThrow("cannot skip the coordinator");
  });

  test("rejects Windows workers and unresolvable targets, but admits a named subset", () => {
    const beta = status({
      fingerprint: "2".repeat(64),
      label: "beta",
      reachableAddr: "beta.example",
    });
    expect(() => _resolveAtomicFleetWorkers(undefined, [
      status({ os: "win32" }),
    ])).toThrow("Windows workers are registered");
    expect(() => _resolveAtomicFleetWorkers("gamma.example", [status()]))
      .toThrow("missing from coordinator worker inventory");
    expect(_resolveAtomicFleetWorkers("alpha.example", [status(), beta]))
      .toEqual([{ fingerprint: "1".repeat(64), host: "alpha.example" }]);
  });
  test("a second orchestration cannot enter while the first owns the fleet lease", async () => {
    const root = mkdtempSync(join(tmpdir(), "roost-fleet-push-lock-"));
    const transactionRoot = join(root, "transactions");
    const releaseRoot = join(root, "releases");
    mkdirSync(transactionRoot, { recursive: true });
    mkdirSync(releaseRoot, { recursive: true });
    const location = {
      journalPath: join(transactionRoot, "coordinator-deploy.json"),
      context: {
        servicePath: join(root, "coord.service"),
        releaseRoot,
        transactionRoot,
        platform: "linux" as const,
      },
    };
    const options = {
      platform: "linux" as const,
      env: { ROOST_SERVICE_DIR: root },
    };
    const first = await acquireFleetPushTransaction(location, options);
    try {
      await expect(acquireFleetPushTransaction(location, options))
        .rejects.toThrow("machine transaction already active");
    } finally {
      await first.release();

      rmSync(root, { recursive: true, force: true });
    }
  });
});
describe("coordinator initial checkpoint failure", () => {
  test("retains referenced artifacts when rename became visible before fsync failed", async () => {
    let cleaned = false;
    await expect(_handleCoordinatorInitialJournalWriteFailure(
      new Error("parent fsync failed"),
      () => ({} as CoordinatorDeployJournalV2),
      async () => { cleaned = true; },
    )).rejects.toThrow("prepared journal is visible");
    expect(cleaned).toBeFalse();
  });

  test("cleans only when the exact journal path is provably absent", async () => {
    let cleaned = false;
    await expect(_handleCoordinatorInitialJournalWriteFailure(
      new Error("write failed before rename"),
      () => null,
      async () => { cleaned = true; },
    )).rejects.toThrow("write failed before rename");
    expect(cleaned).toBeTrue();
  });
});
