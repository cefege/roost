// Atomic fleet rollout regression tests exercise the durable decision boundary.
// They use an in-memory coordinator/worker driver so ordering and exhaustive
// rollback behavior remain visible without mutating host services.

import { describe, expect, test } from "bun:test";
import { mkdirSync, mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
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
  atomicFleetPriorProblems,
  push,
} from "../src/push.ts";
import {
  _handleCoordinatorInitialJournalWriteFailure,
  acquireFleetPushTransaction,
} from "../src/push-coordinator.ts";
import type { CoordinatorDeployJournalV2 } from "../src/coordinator-deploy-journal.ts";
import type { WorkerStatus } from "../src/status.ts";

const PRIOR_SHA = "a".repeat(40);
const TARGET_SHA = "b".repeat(40);
const plan: FleetRolloutPlan = {
  rolloutId: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
  priorSha: PRIOR_SHA,
  targetSha: TARGET_SHA,
  workers: [
    { fingerprint: "1".repeat(64), host: "alpha.example" },
    { fingerprint: "2".repeat(64), host: "beta.example" },
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
    keeperState: "current",
    keeperBuild: null,
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
      .rejects.toThrow("every participant was restored");
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
    await expect(convergeAtomicFleet(plan, deps)).rejects.toThrow("every participant was restored");
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
    await expect(convergeAtomicFleet(plan, deps)).rejects.toThrow("every participant was restored");
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

describe("uniform fleet prior preflight", () => {
  test("rejects drift before a global prior SHA can be journaled", () => {
    expect(atomicFleetPriorProblems([
      status(),
      status({
        fingerprint: "2".repeat(64),
        label: "beta",
        reachableAddr: "beta.example",
        gitSha: "c".repeat(40),
      }),
    ], PRIOR_SHA)).toEqual([
      `beta: reports ${"c".repeat(40)}, prior is ${PRIOR_SHA}`,
    ]);
  });

  test("requires every prior keeper proof to be current", () => {
    expect(atomicFleetPriorProblems([
      status({ keeperState: "unknown" }),
    ], PRIOR_SHA)).toEqual(["alpha: keeper is unknown before rollout"]);
  });
});

describe("fleet journal participant proof", () => {
  test("later registrations do not strand rollback or durable finalization", () => {
    const workers = [
      status(),
      status({
        fingerprint: "2".repeat(64),
        label: "later",
        reachableAddr: "later.example",
      }),
    ];
    expect(_atomicFleetConvergenceProblems(
      workers,
      [plan.workers[0]!],
      PRIOR_SHA,
      "rollback",
      new Map(),
    )).toEqual([]);
    expect(_atomicFleetConvergenceProblems(
      workers.map((worker) => worker.label === "later"
        ? { ...worker, gitSha: TARGET_SHA }
        : worker),
      [plan.workers[0]!],
      PRIOR_SHA,
      "rollback",
      new Map(),
    )).toContain(`later: reports ${TARGET_SHA}, expected ${PRIOR_SHA}`);
    const targetWorkers = workers.map((worker) => ({ ...worker, gitSha: TARGET_SHA }));
    expect(_atomicFleetConvergenceProblems(
      targetWorkers,
      [plan.workers[0]!],
      TARGET_SHA,
      "finalize",
      new Map(),
    )).toEqual([]);
    expect(_atomicFleetConvergenceProblems(
      targetWorkers,
      [plan.workers[0]!],
      TARGET_SHA,
      "hold",
      new Map(),
    )).toContain("registered worker set does not exactly match the rollout journal");
  });
});

describe("fleet push admission", () => {

  test("rejects non-atomic flags before reading repository or fleet state", async () => {
    await expect(push(["--allow-dirty"])).rejects.toThrow("never permits --allow-dirty");
    await expect(push(["--no-coord"])).rejects.toThrow("cannot skip the coordinator");
  });

  test("rejects Windows and partial target sets before orchestration", () => {
    expect(() => _resolveAtomicFleetWorkers(undefined, [
      status({ os: "win32" }),
    ])).toThrow("Windows workers are registered");
    expect(() => _resolveAtomicFleetWorkers("alpha.example", [
      status(),
      status({
        fingerprint: "2".repeat(64),
        label: "beta",
        reachableAddr: "beta.example",
      }),
    ])).toThrow("exact registered worker set");
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
