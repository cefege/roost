// Runtime adapter for journaled `roost push` execution.
// The push owner supplies the admitted plan and pure convergence proof; this
// module binds worker deploys, coordinator checkpoints, and heartbeat polling.

import { resolve } from "node:path";
import {
  beginCoordinatorDeployFinalization,
  finalizeCoordinatorDeploy,
  recoverCoordinatorDeploy,
  VERIFY_POLL_MS,
  VERIFY_TIMEOUT_MS,
} from "./coordinator-deploy-recovery.ts";
import { loadCoordinatorDeployJournal } from "./coordinator-deploy-journal.ts";
import { deploy } from "./deploy.ts";
import type { CoordinatorDeployLocation } from "./push-coordinator.ts";
import type {
  AtomicFleetRolloutDeps,
  FleetRolloutPlan,
  FleetRolloutWorker,
} from "./push-fleet-rollout.ts";
import {
  routableWorkerFingerprints,
  statusReport,
  type WorkerStatus,
} from "./status.ts";
import type { WorkerRolloutAction, WorkerRolloutDirective } from "./worker-deploy-rollout.ts";

const REPO_ROOT = resolve(import.meta.dir, "..", "..", "..");

type FleetConvergenceProblems = (
  workers: readonly WorkerStatus[],
  targets: readonly FleetRolloutWorker[],
  expectedSha: string,
  action: WorkerRolloutAction,
  heartbeatBoundaries: ReadonlyMap<string, number>,
  admissionRecordedAtMs: number,
  routableFingerprints: ReadonlySet<string>,
) => string[];

export function fleetRuntime(
  location: CoordinatorDeployLocation,
  plan: FleetRolloutPlan,
  convergenceProblems: FleetConvergenceProblems,
): AtomicFleetRolloutDeps {
  const heartbeatBoundaries = new Map<WorkerRolloutAction, Map<string, number>>();
  const deployWorker = async (
    worker: FleetRolloutWorker,
    directive: WorkerRolloutDirective,
  ): Promise<void> => {
    let boundaries = heartbeatBoundaries.get(directive.action);
    if (!boundaries) {
      boundaries = new Map();
      heartbeatBoundaries.set(directive.action, boundaries);
    }
    const beforeDeploy = await statusReport();
    const matchingWorkers = beforeDeploy.workers.filter(
      candidate => candidate.fingerprint === worker.fingerprint,
    );
    if (matchingWorkers.length !== 1) {
      throw new Error(
        `${worker.fingerprint}: cannot capture one coordinator heartbeat baseline`,
      );
    }
    boundaries.set(worker.fingerprint, matchingWorkers[0]!.lastSeenMs);
    console.log(`\n>> ${directive.action} worker ${worker.host}`);
    await deploy([
      worker.host,
      `--source-root=${REPO_ROOT}`,
      `--expected-sha=${directive.targetSha}`,
    ], { rollout: directive });
  };
  const proveFleet = async (
    expectedSha: string,
    action: WorkerRolloutAction,
  ): Promise<readonly string[]> => {
    const deadline = Date.now() + VERIFY_TIMEOUT_MS;
    let problems: string[] = [];
    do {
      try {
        const [report, routableFingerprints] = await Promise.all([
          statusReport(),
          routableWorkerFingerprints(),
        ]);
        problems = convergenceProblems(
          report.workers,
          plan.workers,
          expectedSha,
          action,
          heartbeatBoundaries.get(action) ?? new Map(),
          plan.admissionRecordedAtMs,
          routableFingerprints,
        );
        if (problems.length === 0) return [];
      } catch (error) {
        problems = [`coordinator status failed: ${error instanceof Error ? error.message : String(error)}`];
      }
      if (Date.now() < deadline) await Bun.sleep(VERIFY_POLL_MS);
    } while (Date.now() < deadline);
    return problems;
  };
  return {
    deployWorker,
    proveFleet,
    beginCoordinatorFinalization: async () => {
      await beginCoordinatorDeployFinalization(location.journalPath, location.context);
    },
    coordinatorCanRollback: async () =>
      loadCoordinatorDeployJournal(location.journalPath, location.context)?.phase === "fleet-converging",
    finalizeCoordinator: async (finishWorkers) => {
      await finalizeCoordinatorDeploy(
        location.journalPath,
        location.context,
        async () => finishWorkers(),
      );
    },
    rollbackCoordinator: async (rollbackWorkers) => {
      await recoverCoordinatorDeploy(location.journalPath, location.context, {
        rollbackFleet: async () => rollbackWorkers(),
      });
    },
  };
}

