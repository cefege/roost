// Linux worker journal recovery and fleet settlement: the phase state machine
// plus the ssh helpers that load, clear, and prove the TARGET release.
// Prior-release proof and retirement live in linux-prior-service-recovery.ts;
// production command adapters live in deploy-linux-recovery-runtime.ts.
import type { JournaledKeeperUpdateV1 } from "@roost/shared/keeper-update";
import { parsePosixServiceEnvironment } from "./deploy-plist-env.ts";
import {
  DeployFailure, failDeploy, finishWorkerDeploy, workerServiceIsRunning,
  workerServiceMatchesRelease,
} from "./deploy-exec.ts";
import {
  DurableStateRollForwardRequired,
  durableStateRollForwardNotice,
} from "./durable-worker-state.ts";
import {
  linuxDeployRecoveryPlan,
  parseLinuxDeployJournalSnapshot,
} from "./linux-deploy-journal.ts";
import type { LinuxDeployJournal } from "./linux-deploy-journal.ts";
import {
  _linuxCheckpointDeployJournalCommand, _linuxClearDeployJournalCommand,
  _linuxLoadDeployJournalCommand, _linuxRemoveManagedWorkerReleaseCommand,
  _linuxTargetVerificationCommand, _linuxWorkerShaProofCommand,
} from "./linux-deploy-journal-commands.ts";
import type { WorkerRolloutDirective } from "./worker-deploy-rollout.ts";
import { assertWorkerRolloutDirective, assertWorkerRolloutMatches } from "./worker-deploy-rollout.ts";
export type LinuxDeploySsh = (command: string) =>
  Promise<{ exit: number; stdout: string; stderr: string }>;
export type LinuxKeeperUpdateDirection = "target" | "source";
export type ApplyLinuxKeeperUpdate = (
  workerFingerprint: string,
  keeperUpdate: Readonly<JournaledKeeperUpdateV1>,
  direction: LinuxKeeperUpdateDirection,
  actionReleasePath: string,
) => Promise<void>;
export type ProveLinuxKeeperUpdate = (
  workerFingerprint: string,
  keeperUpdate: Readonly<JournaledKeeperUpdateV1>,
  direction: LinuxKeeperUpdateDirection,
  expectedWorkerSha: string,
  heartbeatNotBeforeMs: number,
  actionReleasePath: string,
) => Promise<void>;
export interface LinuxDeployTargetProof {
  healthy: boolean;
  proof: { exit: number; stdout: string; stderr: string };
}

export interface LinuxDeployRecoveryRemote {
  home: string;
  loadJournal: () => Promise<LinuxDeployJournal | null>;
  proveTarget: (journal: LinuxDeployJournal) => Promise<LinuxDeployTargetProof>;
  /** Records the durable-state version the target left behind and returns the
   * refreshed journal, so an impossible rollback can prove why. */
  checkpointRollback: (journal: LinuxDeployJournal) => Promise<LinuxDeployJournal>;
  stopWorker: (journal: LinuxDeployJournal) => Promise<void>;
  startWorker: (journal: LinuxDeployJournal) => Promise<void>;
  checkpointCommit: (journal: LinuxDeployJournal) => Promise<void>;
  applyKeeperUpdate: ApplyLinuxKeeperUpdate;
  proveKeeperUpdate: ProveLinuxKeeperUpdate;
  restorePrior: (journal: LinuxDeployJournal) => Promise<void>;
  settlePrior: (journal: LinuxDeployJournal) => Promise<void>;
  provePriorWorker: (journal: LinuxDeployJournal, expectedSha: string) => Promise<void>;
  provePrior: (journal: LinuxDeployJournal, priorStarted: boolean) => Promise<void>;
  cleanupPrior: (journal: LinuxDeployJournal) => Promise<void>;
  removeTarget: (journal: LinuxDeployJournal) => Promise<void>;
  clearJournal: () => Promise<void>;
  now?: () => number;
}

export type LinuxRecoveryOutcome =
  | { kind: "none" }
  | { kind: "prepared-cleaned"; journal: LinuxDeployJournal }
  | { kind: "target-held"; journal: LinuxDeployJournal; verification: { exit: number; stdout: string; stderr: string } }
  | { kind: "target-committed"; journal: LinuxDeployJournal; verification: { exit: number; stdout: string; stderr: string } }
  | { kind: "prior-restored"; journal: LinuxDeployJournal }
  | { kind: "roll-forward-required"; journal: LinuxDeployJournal; reason: string };
export async function settleInitialLinuxRecovery(
  host: string,
  recovery: LinuxRecoveryOutcome,
  rollout: Readonly<WorkerRolloutDirective> | null,
  deploySsh: LinuxDeploySsh,
  journalPath: string,
): Promise<boolean> {
  if (recovery.kind === "target-held") {
    if (rollout?.action !== "hold") {
      failDeploy(5, "a fleet-held Linux worker requires its owning rollout");
    }
    if (recovery.journal.phase === "activating") {
      const checkpoint = await deploySsh(
        _linuxCheckpointDeployJournalCommand(journalPath, "activating", "activated"),
      );
      if (checkpoint.exit !== 0) {
        failDeploy(checkpoint.exit || 5, "cannot checkpoint recovered Linux fleet activation");
      }
    }
    finishWorkerDeploy(
      recovery.verification,
      `>> held ${host} v2 worker for fleet rollout ${rollout.rolloutId}`,
      "linux",
    );
    return true;
  }
  if (recovery.kind === "none"
    && (rollout?.action === "finalize" || rollout?.action === "rollback")) {
    const expectedSha = rollout.action === "finalize" ? rollout.targetSha : rollout.priorSha;
    const proof = await deploySsh(_linuxWorkerShaProofCommand(expectedSha));
    if (proof.exit !== 0 || !workerServiceIsRunning(proof.stdout, "linux")
      || !/^RoostGitShaMatch=yes$/m.test(proof.stdout)) {
      failDeploy(proof.exit || 5, `Linux worker has no journal and does not prove ${expectedSha}`);
    }
    const settlement = rollout.action === "finalize" ? "finalized" : "rolled back";
    finishWorkerDeploy(proof, `>> Linux worker already ${settlement} on ${host}`, "linux");
    return true;
  }
  if (rollout?.action === "finalize") {
    if (recovery.kind !== "target-committed") {
      failDeploy(5, "Linux worker target was not finalized");
    }
    finishWorkerDeploy(recovery.verification, `>> finalized fleet worker ${host}`, "linux");
    return true;
  }
  if (rollout?.action === "rollback") {
    if (recovery.kind !== "prior-restored" && recovery.kind !== "prepared-cleaned") {
      failDeploy(5, "Linux worker prior state was not restored");
    }
    console.log(`>> rolled back fleet worker ${host}`);
    return true;
  }
  if (recovery.kind === "prepared-cleaned") {
    console.log(">> recovered interrupted Linux deploy (discarded prepared stage)");
  } else if (recovery.kind === "target-committed") {
    console.log(">> recovered interrupted Linux deploy (verified activated target)");
  } else if (recovery.kind === "prior-restored") {
    console.log(">> recovered interrupted Linux deploy (restored prior service)");
  } else if (recovery.kind === "roll-forward-required") {
    console.log(">> cleared a Linux deploy journal whose rollback was impossible; rolling forward");
  }
  return false;
}
export async function loadLinuxDeployJournal(
  deploySsh: LinuxDeploySsh,
  journalPath: string,
  home: string,
): Promise<LinuxDeployJournal | null> {
  const loaded = await deploySsh(_linuxLoadDeployJournalCommand(journalPath));
  if (loaded.exit !== 0) {
    failDeploy(loaded.exit || 5, `cannot read the fixed Linux deployment journal; it was left intact\n${loaded.stdout}\n${loaded.stderr}`);
  }
  return parseLinuxDeployJournalSnapshot(loaded.stdout, home);
}

export async function removeManagedLinuxWorkerRelease(
  deploySsh: LinuxDeploySsh,
  targetReleasePath: string,
  home: string,
): Promise<void> {
  const removed = await deploySsh(_linuxRemoveManagedWorkerReleaseCommand(targetReleasePath, home));
  if (removed.exit !== 0) {
    failDeploy(removed.exit || 5, `cannot remove managed worker stage ${targetReleasePath}; deployment journal retained\n${removed.stdout}\n${removed.stderr}`);
  }
}

export async function clearLinuxDeployJournal(
  deploySsh: LinuxDeploySsh,
  journalPath: string,
): Promise<void> {
  const cleared = await deploySsh(_linuxClearDeployJournalCommand(journalPath));
  if (cleared.exit !== 0) {
    failDeploy(cleared.exit || 5, `cannot durably clear the Linux deployment journal\n${cleared.stdout}\n${cleared.stderr}`);
  }
}

export async function proveLinuxTargetRelease(
  deploySsh: LinuxDeploySsh,
  journal: LinuxDeployJournal,
  home: string,
): Promise<LinuxDeployTargetProof> {
  let proof = { exit: 1, stdout: "", stderr: "target verification was not attempted" };
  for (let attempt = 0; attempt < 20; attempt += 1) {
    proof = await deploySsh(_linuxTargetVerificationCommand(journal, home));
    if (proof.exit === 0
      && workerServiceIsRunning(proof.stdout, "linux")
      && workerServiceMatchesRelease(proof.stdout)) return { healthy: true, proof };
    if (proof.exit === 9 || proof.exit === 130 || proof.exit === 143) break;
    if (attempt < 19) await Bun.sleep(250);
  }
  return { healthy: false, proof };
}


export async function _recoverLinuxDeployJournal(
  remote: LinuxDeployRecoveryRemote,
  directive?: Readonly<WorkerRolloutDirective>,
): Promise<LinuxRecoveryOutcome> {
  const journal = await remote.loadJournal();
  if (journal === null) return { kind: "none" };
  const requested = directive ? assertWorkerRolloutDirective(directive) : null;
  if (requested) assertWorkerRolloutMatches(journal, requested);
  if (journal.rolloutId !== null && !requested) failDeploy(5, "a fleet rollout still owns the Linux worker deploy journal");
  if (requested) {
    const priorEnvironment = journal.priorUnit ? parsePosixServiceEnvironment(journal.priorUnit, "linux") : {};
    const priorSha = priorEnvironment.GIT_SHA ?? priorEnvironment.ROOST_GIT_SHA;
    if (priorSha?.toLowerCase() !== requested.priorSha) failDeploy(5, "Linux worker journal does not prove the fleet rollout prior identity");
  }
  if (journal.phase === "prepared") {
    if (requested?.action === "finalize") failDeploy(5, "cannot finalize a Linux worker before activation");
    await remote.removeTarget(journal);
    await remote.clearJournal();
    return { kind: "prepared-cleaned", journal };
  }
  if (requested && journal.priorLifecycle !== "running") failDeploy(5, "Linux worker journal does not prove the fleet rollout prior identity");
  const applyRecordedKeeperUpdate = async (
    direction: LinuxKeeperUpdateDirection,
  ): Promise<void> => {
    if (!journal.keeperUpdate) return;
    await remote.applyKeeperUpdate(
      journal.workerFingerprint!,
      journal.keeperUpdate,
      direction,
      journal.targetReleasePath,
    );
  };
  const replayTargetKeeperUpdate = async (): Promise<LinuxDeployTargetProof> => {
    if (!journal.keeperUpdate) return await remote.proveTarget(journal);
    await remote.stopWorker(journal);
    await remote.startWorker(journal);
    await applyRecordedKeeperUpdate("target");
    await remote.stopWorker(journal);
    const heartbeatNotBeforeMs = (remote.now ?? Date.now)();
    await remote.startWorker(journal);
    const restarted = await remote.proveTarget(journal);
    if (!restarted.healthy) {
      throw new DeployFailure(
        restarted.proof.exit || 5,
        "Linux target did not recover after replaying its keeper update action",
      );
    }
    await remote.proveKeeperUpdate(
      journal.workerFingerprint!,
      journal.keeperUpdate,
      "target",
      journal.targetSha,
      heartbeatNotBeforeMs,
      journal.targetReleasePath,
    );
    return restarted;
  };
  const restorePrior = async (): Promise<LinuxRecoveryOutcome> => {
    const priorEnvironment = journal.keeperUpdate
      ? parsePosixServiceEnvironment(journal.priorUnit!, "linux")
      : {};
    const priorSha = priorEnvironment.GIT_SHA ?? priorEnvironment.ROOST_GIT_SHA ?? null;
    if (journal.keeperUpdate && (!priorSha || !/^[a-f0-9]{40,64}$/i.test(priorSha))) {
      throw new DeployFailure(5, "Linux rollback journal lacks the prior worker identity");
    }
    const rollingBack = journal.phase === "rolling-back"
      ? journal
      : await remote.checkpointRollback(journal);
    await remote.stopWorker(journal);
    // A present prior unit is restored AND started by this command; an absent
    // one has nothing to start, so it can never be proven unrunnable.
    await remote.restorePrior(journal);
    const priorStarted = journal.priorUnit !== null;
    if (journal.keeperUpdate) {
      await applyRecordedKeeperUpdate("source");
      await remote.stopWorker(journal);
      const heartbeatNotBeforeMs = (remote.now ?? Date.now)();
      await remote.startWorker(journal);
      await remote.provePriorWorker(journal, priorSha!);
      await remote.proveKeeperUpdate(
        journal.workerFingerprint!,
        journal.keeperUpdate,
        "source",
        priorSha!,
        heartbeatNotBeforeMs,
        journal.targetReleasePath,
      );
    }
    await remote.settlePrior(journal);
    try {
      await remote.provePrior(rollingBack, priorStarted);
    } catch (error) {
      if (!(error instanceof DurableStateRollForwardRequired)) throw error;
      const reason = durableStateRollForwardNotice(error, rollingBack.targetReleasePath);
      console.error(reason);
      // The staged target is the only release that can still run, so it stays
      // while the journal goes: a retained journal would fail this same proof
      // on every later deploy.
      await remote.clearJournal();
      return { kind: "roll-forward-required", journal: rollingBack, reason };
    }
    await remote.removeTarget(journal);
    await remote.clearJournal();
    return { kind: "prior-restored", journal: rollingBack };
  };
  if (journal.phase === "rolling-back") {
    if (requested?.action === "finalize") {
      throw new DeployFailure(5, "cannot finalize a Linux worker after rollback was chosen");
    }
    return await restorePrior();
  }
  if (journal.phase === "committing" && requested?.action === "rollback") {
    throw new DeployFailure(5, "cannot roll back a Linux worker after target commit was chosen");
  }
  if (requested?.action === "rollback") return await restorePrior();
  if (journal.rolloutId !== null && requested?.action === "finalize"
    && journal.phase !== "activated" && journal.phase !== "committing") {
    throw new DeployFailure(5, "cannot finalize a Linux worker before activation");
  }
  let target: LinuxDeployTargetProof;
  try {
    target = await replayTargetKeeperUpdate();
  } catch (error) {
    if (requested?.action === "finalize" || journal.phase === "committing") throw error;
    return await restorePrior();
  }
  if (journal.phase === "committing") {
    await remote.cleanupPrior(journal);
    await remote.clearJournal();
    return { kind: "target-committed", journal, verification: target.proof };
  }
  if (journal.rolloutId !== null) {
    if (requested?.action === "finalize") {
      await remote.checkpointCommit(journal);
      await remote.cleanupPrior(journal);
      await remote.clearJournal();
      return { kind: "target-committed", journal, verification: target.proof };
    }
    if (target.healthy) return { kind: "target-held", journal, verification: target.proof };
    return await restorePrior();
  }
  const plan = linuxDeployRecoveryPlan(journal, target.healthy, remote.home);
  if (plan.kind === "commit-target") {
    await remote.checkpointCommit(journal);
    await remote.cleanupPrior(journal);
    await remote.clearJournal();
    return { kind: "target-committed", journal, verification: target.proof };
  }
  return await restorePrior();
}
