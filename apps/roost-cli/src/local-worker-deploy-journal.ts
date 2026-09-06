// Recovery decisions and phase outcomes for localhost worker deploy journals.
// The schema module owns parsing, confinement, and service metadata checks.
// deploy-local.ts supplies durable IO and concrete service lifecycle actions.

import { join, resolve } from "node:path";
import type { JournaledKeeperUpdateV1 } from "@roost/shared/keeper-update";
import { roostServiceDir } from "@roost/shared/paths";
import {
  decodeServiceSnapshot,
  LOCAL_WORKER_DEPLOY_JOURNAL_SCHEMA_VERSION,
  localWorkerDeployStageIsConfined,
  localWorkerReleaseMatches,
  normalizedMetadataPath,
  parseLocalWorkerDeployJournal,
  serviceGitSha,
  serviceSnapshotMatches,
  serviceWorkingDirectory,
} from "./local-worker-deploy-journal-schema.ts";
import type {
  LocalWorkerDeployConfinement,
  LocalWorkerDeployJournal,
  LocalWorkerDeployPhase,
  LocalWorkerLifecycle,
  LocalWorkerServiceSnapshot,
  LocalWorkerStartupPolicy,
} from "./local-worker-deploy-journal-schema.ts";
import { posixDeployJournalDecision } from "./posix-deploy-journal.ts";
import {
  assertWorkerRolloutDirective,
  assertWorkerRolloutMatches,
} from "./worker-deploy-rollout.ts";
import type { WorkerRolloutDirective } from "./worker-deploy-rollout.ts";

export {
  decodeServiceSnapshot,
  LOCAL_WORKER_DEPLOY_JOURNAL_SCHEMA_VERSION,
  localWorkerDeployStageIsConfined,
  localWorkerReleaseMatches,
  normalizedMetadataPath,
  parseLocalWorkerDeployJournal,
  serviceGitSha,
  serviceSnapshotMatches,
  serviceWorkingDirectory,
};
export type {
  LocalWorkerDeployConfinement,
  LocalWorkerDeployJournal,
  LocalWorkerDeployPhase,
  LocalWorkerLifecycle,
  LocalWorkerServiceSnapshot,
  LocalWorkerStartupPolicy,
};

const LOCAL_WORKER_DEPLOY_JOURNAL_FILE = "worker-deploy.json";
export interface LocalWorkerDeployRecoveryDeps {
  readService: (journal: Readonly<LocalWorkerDeployJournal>) =>
    LocalWorkerServiceSnapshot | null | Promise<LocalWorkerServiceSnapshot | null>;
  probeLifecycle: (journal: Readonly<LocalWorkerDeployJournal>) =>
    LocalWorkerLifecycle | Promise<LocalWorkerLifecycle>;
  probeStartupPolicy: (journal: Readonly<LocalWorkerDeployJournal>) =>
    LocalWorkerStartupPolicy | Promise<LocalWorkerStartupPolicy>;
  checkpointRollback: (journal: Readonly<LocalWorkerDeployJournal>) => Promise<void>;
  checkpointCommit: (journal: Readonly<LocalWorkerDeployJournal>) => Promise<void>;
  restorePriorDefinition: (journal: Readonly<LocalWorkerDeployJournal>) => Promise<void>;
  stopWorker: (journal: Readonly<LocalWorkerDeployJournal>) => Promise<void>;
  startPrior: (journal: Readonly<LocalWorkerDeployJournal>) => Promise<void>;
  restorePriorLifecycle: (journal: Readonly<LocalWorkerDeployJournal>) => Promise<void>;
  activateTarget: (journal: Readonly<LocalWorkerDeployJournal>) => Promise<void>;
  applyKeeperUpdate: (
    workerFingerprint: string,
    update: Readonly<JournaledKeeperUpdateV1>,
    direction: "target" | "source",
  ) => Promise<void>;
  proveKeeperUpdate: (
    workerFingerprint: string,
    update: Readonly<JournaledKeeperUpdateV1>,
    direction: "target" | "source",
    expectedWorkerSha: string,
    heartbeatNotBeforeMs: number,
  ) => Promise<void>;
  cleanupStage: (journal: Readonly<LocalWorkerDeployJournal>) => Promise<void>;
  commitTarget: (journal: Readonly<LocalWorkerDeployJournal>) => Promise<void>;
  clearJournal: () => Promise<void>;
  sleep?: (milliseconds: number) => Promise<void>;
  proofAttempts?: number;
  now?: () => number;
}
export type LocalWorkerDeployRecoveryDecision =
  | "prepared-cleaned"
  | "target-held"
  | "target-committed"
  | "prior-restored";

export async function priorServiceIsProven(
  journal: Readonly<LocalWorkerDeployJournal>,
  deps: Readonly<LocalWorkerDeployRecoveryDeps>,
): Promise<boolean> {
  const attempts = Math.max(1, deps.proofAttempts ?? 20);
  const sleep = deps.sleep ?? Bun.sleep;
  for (let attempt = 0; attempt < attempts; attempt += 1) {
    try {
      const definition = await deps.readService(journal);
      const lifecycle = await deps.probeLifecycle(journal);
      const startupPolicy = await deps.probeStartupPolicy(journal);
      if (
        serviceSnapshotMatches(definition, journal.priorService)
        && lifecycle === journal.priorLifecycle
        && startupPolicy === journal.priorStartupPolicy
      ) {
        return true;
      }
    } catch {
      // A transient read/probe failure is retried; exhaustion fails closed.
    }
    if (attempt + 1 < attempts) await sleep(250);
  }
  return false;
}

export async function _rollbackLocalWorkerDeployJournal(
  journal: Readonly<LocalWorkerDeployJournal>,
  deps: Readonly<LocalWorkerDeployRecoveryDeps>,
): Promise<LocalWorkerDeployRecoveryDecision> {
  const rollbackJournal: LocalWorkerDeployJournal = journal.phase === "rolling-back"
    ? { ...journal }
    : { ...journal, phase: "rolling-back" };
  if (journal.phase !== "rolling-back") {
    await deps.checkpointRollback(rollbackJournal);
  }
  await deps.stopWorker(rollbackJournal);
  await deps.restorePriorDefinition(rollbackJournal);
  if (rollbackJournal.keeperUpdate) {
    if (!rollbackJournal.priorGitSha) {
      throw new Error("rollback keeper update lacks the prior worker identity");
    }
    await deps.startPrior(rollbackJournal);
    await deps.applyKeeperUpdate(
      rollbackJournal.workerFingerprint!,
      rollbackJournal.keeperUpdate,
      "source",
    );
    await deps.stopWorker(rollbackJournal);
    const heartbeatNotBeforeMs = (deps.now ?? Date.now)();
    await deps.startPrior(rollbackJournal);
    await deps.proveKeeperUpdate(
      rollbackJournal.workerFingerprint!,
      rollbackJournal.keeperUpdate,
      "source",
      rollbackJournal.priorGitSha,
      heartbeatNotBeforeMs,
    );
  }
  await deps.restorePriorLifecycle(rollbackJournal);
  if (!await priorServiceIsProven(rollbackJournal, deps)) {
    throw new Error("prior worker service definition and lifecycle could not be proven");
  }
  await deps.cleanupStage(rollbackJournal);
  await deps.clearJournal();
  return "prior-restored";
}

export async function _recoverLocalWorkerDeployJournal(
  raw: string,
  confinement: Readonly<LocalWorkerDeployConfinement>,
  deps: Readonly<LocalWorkerDeployRecoveryDeps>,
  directive?: Readonly<WorkerRolloutDirective>,
): Promise<LocalWorkerDeployRecoveryDecision> {
  const journal = parseLocalWorkerDeployJournal(raw, confinement);
  const requested = directive ? assertWorkerRolloutDirective(directive) : null;
  if (requested) assertWorkerRolloutMatches(journal, requested);
  if (journal.rolloutId !== null && !requested) {
    throw new Error("a fleet rollout still owns the local worker deploy journal");
  }
  if (requested && journal.priorGitSha?.toLowerCase() !== requested.priorSha) {
    throw new Error("local worker journal does not prove the fleet rollout prior identity");
  }
  if (journal.phase === "prepared") {
    if (requested?.action === "finalize") {
      throw new Error("cannot finalize a worker rollout before activation");
    }
    await deps.cleanupStage(journal);
    await deps.clearJournal();
    return "prepared-cleaned";
  }
  let targetIsProven = false;
  if (journal.targetService) {
    try {
      const activeService = await deps.readService(journal);
      if (serviceSnapshotMatches(activeService, journal.targetService)) {
        targetIsProven = await deps.probeLifecycle(journal) === "running";
      }
    } catch {
      // Any failure to prove the exact target falls through to rollback.
    }
  }

  const applyRecordedKeeperUpdate = async (
    direction: "target" | "source",
  ): Promise<void> => {
    if (!journal.keeperUpdate) return;
    await deps.applyKeeperUpdate(
      journal.workerFingerprint!,
      journal.keeperUpdate,
      direction,
    );
  };
  const replayTargetKeeperUpdate = async (): Promise<void> => {
    if (!journal.keeperUpdate) return;
    await deps.stopWorker(journal);
    await deps.activateTarget(journal);
    await applyRecordedKeeperUpdate("target");
    await deps.stopWorker(journal);
    const heartbeatNotBeforeMs = (deps.now ?? Date.now)();
    await deps.activateTarget(journal);
    const activeService = await deps.readService(journal);
    const lifecycle = await deps.probeLifecycle(journal);
    if (!journal.targetService
      || !serviceSnapshotMatches(activeService, journal.targetService)
      || lifecycle !== "running") {
      throw new Error("target worker service could not be proven after keeper update replay");
    }
    await deps.proveKeeperUpdate(
      journal.workerFingerprint!,
      journal.keeperUpdate,
      "target",
      journal.targetSha,
      heartbeatNotBeforeMs,
    );
  };
  if (journal.phase === "rolling-back") {
    if (requested?.action === "finalize") {
      throw new Error("cannot finalize a local worker after rollback was chosen");
    }
    return await _rollbackLocalWorkerDeployJournal(journal, deps);
  }
  if (journal.phase === "committing") {
    if (requested?.action === "rollback") {
      throw new Error("cannot roll back a local worker after target commit was chosen");
    }
    await replayTargetKeeperUpdate();
    await deps.commitTarget(journal);
    await deps.clearJournal();
    return "target-committed";
  }
  if (requested?.action === "rollback") {
    return await _rollbackLocalWorkerDeployJournal(journal, deps);
  }
  if (journal.rolloutId !== null) {
    if (requested?.action === "finalize") {
      if (journal.phase !== "activated") {
        throw new Error("cannot finalize a worker rollout before its activated checkpoint");
      }
      await replayTargetKeeperUpdate();
      await deps.checkpointCommit({ ...journal, phase: "committing" });
      await deps.commitTarget(journal);
      await deps.clearJournal();
      return "target-committed";
    }
    if (!targetIsProven) {
      return await _rollbackLocalWorkerDeployJournal(journal, deps);
    }
    try {
      await replayTargetKeeperUpdate();
    } catch {
      return await _rollbackLocalWorkerDeployJournal(journal, deps);
    }
    return "target-held";
  }
  const decision = posixDeployJournalDecision(journal.phase, targetIsProven);
  if (decision === "commit") {
    try {
      await replayTargetKeeperUpdate();
    } catch {
      return await _rollbackLocalWorkerDeployJournal(journal, deps);
    }
    await deps.checkpointCommit({ ...journal, phase: "committing" });
    await deps.commitTarget(journal);
    await deps.clearJournal();
    return "target-committed";
  }
  return await _rollbackLocalWorkerDeployJournal(journal, deps);
}

export function localWorkerDeployJournalPath(serviceDir: string = roostServiceDir()): string {
  return join(resolve(serviceDir), "transactions", LOCAL_WORKER_DEPLOY_JOURNAL_FILE);
}
