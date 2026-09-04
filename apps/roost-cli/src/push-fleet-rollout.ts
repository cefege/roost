// Atomic worker-fleet convergence after a coordinator target is held.
// Push supplies concrete deploy/proof and coordinator journal operations; this
// module owns the irreversible decision boundary and exhaustive fan-out rules.

import { DeployFailure } from "./deploy-exec.ts";
import type { WorkerRolloutAction, WorkerRolloutDirective } from "./worker-deploy-rollout.ts";

export interface FleetRolloutWorker {
  fingerprint: string;
  host: string;
}

export interface FleetRolloutPlan {
  rolloutId: string;
  priorSha: string;
  targetSha: string;
  workers: readonly FleetRolloutWorker[];
}

export interface AtomicFleetRolloutDeps {
  deployWorker: (
    worker: FleetRolloutWorker,
    directive: WorkerRolloutDirective,
  ) => Promise<void>;
  proveFleet: (
    expectedSha: string,
    action: WorkerRolloutAction,
  ) => Promise<readonly string[]>;
  beginCoordinatorFinalization: () => Promise<void>;
  coordinatorCanRollback: () => Promise<boolean>;
  finalizeCoordinator: (finishWorkers: () => Promise<void>) => Promise<void>;
  rollbackCoordinator: (rollbackWorkers: () => Promise<void>) => Promise<void>;
}
export type InterruptedFleetRecoveryAction =
  | "coordinator-rollback"
  | "converge-target"
  | "rollback-fleet"
  | "finish-target";

export function interruptedFleetRecoveryAction(
  phase: "prepared" | "activating" | "fleet-converging" | "finalizing",
  requestedTargetMatches: boolean,
): InterruptedFleetRecoveryAction {
  if (phase === "finalizing") return "finish-target";
  if (phase === "fleet-converging") {
    return requestedTargetMatches ? "converge-target" : "rollback-fleet";
  }
  return "coordinator-rollback";
}


function directiveFor(
  plan: FleetRolloutPlan,
  action: WorkerRolloutAction,
): WorkerRolloutDirective {
  return {
    action,
    rolloutId: plan.rolloutId,
    priorSha: plan.priorSha,
    targetSha: plan.targetSha,
  };
}

async function settleEveryWorker(
  plan: FleetRolloutPlan,
  action: WorkerRolloutAction,
  deps: AtomicFleetRolloutDeps,
): Promise<string[]> {
  const directive = directiveFor(plan, action);
  const outcomes = await Promise.all(plan.workers.map(async (worker) => {
    try {
      await deps.deployWorker(worker, directive);
      return null;
    } catch (error) {
      const detail = error instanceof Error ? error.message : String(error);
      return `${worker.fingerprint} (${worker.host}): ${detail}`;
    }
  }));
  return outcomes.filter((problem): problem is string => problem !== null);
}

async function proveOrProblems(
  expectedSha: string,
  action: WorkerRolloutAction,
  deps: AtomicFleetRolloutDeps,
): Promise<string[]> {
  try {
    return [...await deps.proveFleet(expectedSha, action)];
  } catch (error) {
    return [`fleet proof failed: ${error instanceof Error ? error.message : String(error)}`];
  }
}

function settlementFailure(action: WorkerRolloutAction, problems: readonly string[]): DeployFailure {
  return new DeployFailure(
    8,
    `fleet ${action} did not settle every worker:\n${problems.join("\n")}`,
  );
}

async function settleAndProve(
  plan: FleetRolloutPlan,
  action: WorkerRolloutAction,
  expectedSha: string,
  deps: AtomicFleetRolloutDeps,
): Promise<void> {
  const problems = await settleEveryWorker(plan, action, deps);
  problems.push(...await proveOrProblems(expectedSha, action, deps));
  if (problems.length > 0) throw settlementFailure(action, problems);
}

/** Roll back every target before restoring the coordinator, then prove again after restore. */
export async function rollbackAtomicFleet(
  plan: FleetRolloutPlan,
  deps: AtomicFleetRolloutDeps,
): Promise<void> {
  await deps.rollbackCoordinator(async () => {
    const problems = await settleEveryWorker(plan, "rollback", deps);
    if (problems.length > 0) throw settlementFailure("rollback", problems);
  });
  const restoredProblems = await proveOrProblems(plan.priorSha, "rollback", deps);
  if (restoredProblems.length > 0) throw settlementFailure("rollback", restoredProblems);
}

/** Finish the durable global commit decision. This path can never switch to rollback. */
export async function finishAtomicFleetFinalization(
  plan: FleetRolloutPlan,
  deps: AtomicFleetRolloutDeps,
): Promise<void> {
  await deps.finalizeCoordinator(async () => {
    await settleAndProve(plan, "finalize", plan.targetSha, deps);
  });
}

/** Converge all held workers, persist the commit decision, then finalize every participant. */
export async function convergeAtomicFleet(
  plan: FleetRolloutPlan,
  deps: AtomicFleetRolloutDeps,
): Promise<void> {
  let finalizing = false;
  let decisionPending = false;
  try {
    await settleAndProve(plan, "hold", plan.targetSha, deps);
    decisionPending = true;
    await deps.beginCoordinatorFinalization();
    decisionPending = false;
    finalizing = true;
    await finishAtomicFleetFinalization(plan, deps);
  } catch (forwardError) {
    if (decisionPending) {
      let canRollback = false;
      try {
        canRollback = await deps.coordinatorCanRollback();
      } catch {
        throw forwardError;
      }
      if (!canRollback) throw forwardError;
    }
    if (finalizing) throw forwardError;
    try {
      await rollbackAtomicFleet(plan, deps);
    } catch (rollbackError) {
      throw new DeployFailure(
        8,
        `fleet rollout failed and full rollback is incomplete:\n` +
          `${forwardError instanceof Error ? forwardError.message : String(forwardError)}\n` +
          `${rollbackError instanceof Error ? rollbackError.message : String(rollbackError)}`,
      );
    }
    throw new DeployFailure(
      8,
      `fleet rollout failed and every participant was restored to ${plan.priorSha}:\n` +
        `${forwardError instanceof Error ? forwardError.message : String(forwardError)}`,
    );
  }
}
