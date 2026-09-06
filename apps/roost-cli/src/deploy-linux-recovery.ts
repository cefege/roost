// Pure Linux worker journal recovery and fleet settlement.
// The source deploy driver supplies leased SSH operations; production command
// adapters live in deploy-linux-recovery-runtime.ts.
import { posix } from "node:path";
import type { JournaledKeeperUpdateV1 } from "@roost/shared/keeper-update";
import { parsePosixServiceEnvironment, parseSystemdServiceDirective } from "./deploy-plist-env.ts";
import {
  DeployFailure, failDeploy, finishWorkerDeploy, workerServiceIsRunning,
  workerServiceMatchesRelease,
} from "./deploy-exec.ts";
import { COORD_UNIT } from "./service-ctl.ts";
import {
  isManagedLinuxWorkerReleasePath, linuxDeployRecoveryPlan,
  parseLinuxDeployJournalSnapshot,
} from "./linux-deploy-journal.ts";
import type { LinuxDeployJournal } from "./linux-deploy-journal.ts";
import {
  _linuxCheckpointDeployJournalCommand, _linuxClearDeployJournalCommand,
  _linuxLoadDeployJournalCommand, _linuxPriorServiceProofCommand,
  _linuxRemoveManagedWorkerReleaseCommand, _linuxRestorePriorServiceCommand,
  _linuxStartWorkerServiceCommand, _linuxStopWorkerServiceCommand,
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
export function shouldRemovePriorWorkerRelease(
  prior: string,
  current: string,
  coordinator: string | null,
  home: string,
): boolean {
  if ((coordinator !== null && (!posix.isAbsolute(coordinator) || /[\r\n\0]/.test(coordinator)))
    || !prior || prior === current || prior === coordinator
    || !isManagedLinuxWorkerReleasePath(prior, home)
    || !isManagedLinuxWorkerReleasePath(current, home)) return false;
  return true;
}

export function linuxCoordinatorWorkingDirectoryCommand(): string {
  return `set -e; export XDG_RUNTIME_DIR="\${XDG_RUNTIME_DIR:-/run/user/\$(id -u)}"; ` +
    `load_state=$(systemctl --user show ${COORD_UNIT} --property=LoadState --value); ` +
    `case "$load_state" in not-found) printf 'absent\\n';; ` +
    `loaded) systemctl --user show ${COORD_UNIT} --property=WorkingDirectory --value;; *) exit 65;; esac`;
}

export interface LinuxDeployTargetProof {
  healthy: boolean;
  proof: { exit: number; stdout: string; stderr: string };
}

export interface LinuxDeployRecoveryRemote {
  home: string;
  loadJournal: () => Promise<LinuxDeployJournal | null>;
  proveTarget: (journal: LinuxDeployJournal) => Promise<LinuxDeployTargetProof>;
  checkpointRollback: (journal: LinuxDeployJournal) => Promise<void>;
  stopWorker: (journal: LinuxDeployJournal) => Promise<void>;
  startWorker: (journal: LinuxDeployJournal) => Promise<void>;
  checkpointCommit: (journal: LinuxDeployJournal) => Promise<void>;
  applyKeeperUpdate: ApplyLinuxKeeperUpdate;
  proveKeeperUpdate: ProveLinuxKeeperUpdate;
  restorePrior: (journal: LinuxDeployJournal) => Promise<void>;
  settlePrior: (journal: LinuxDeployJournal) => Promise<void>;
  provePriorWorker: (journal: LinuxDeployJournal, expectedSha: string) => Promise<void>;
  provePrior: (journal: LinuxDeployJournal) => Promise<void>;
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
  | { kind: "prior-restored"; journal: LinuxDeployJournal };
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

export async function proveLinuxPriorService(
  deploySsh: LinuxDeploySsh,
  journal: LinuxDeployJournal,
  journalPath: string,
  unitPath: string,
  home: string,
): Promise<void> {
  let proof = { exit: 1, stdout: "", stderr: "rollback verification was not attempted" };
  for (let attempt = 0; attempt < 20; attempt += 1) {
    proof = await deploySsh(_linuxPriorServiceProofCommand(journal, journalPath, unitPath, home));
    const lifecycleMatches = /^RoostPriorStateMatch=yes$/m.test(proof.stdout);
    const runningMatches = journal.priorLifecycle !== "running"
      || workerServiceIsRunning(proof.stdout, "linux");
    if (proof.exit === 0 && lifecycleMatches && runningMatches) return;
    if (proof.exit === 9 || proof.exit === 130 || proof.exit === 143) break;
    if (attempt < 19) await Bun.sleep(250);
  }
  failDeploy(proof.exit || 5, `rollback could not prove the exact prior unit and lifecycle; deployment journal retained\n${proof.stdout}\n${proof.stderr}`);
}

export async function removePriorLinuxWorkerRelease(
  deploySsh: LinuxDeploySsh,
  journal: LinuxDeployJournal,
  home: string,
  signal: AbortSignal,
): Promise<void> {
  const prior = journal.priorUnit === null
    ? ""
    : parseSystemdServiceDirective(journal.priorUnit, "WorkingDirectory") ?? "";
  if (!prior || prior === journal.targetReleasePath
    || !shouldRemovePriorWorkerRelease(prior, journal.targetReleasePath, "/dev/null", home)) return;
  const coordinator = await deploySsh(linuxCoordinatorWorkingDirectoryCommand());
  if (signal.aborted) {
    const reason = signal.reason;
    throw reason instanceof DeployFailure
      ? reason
      : new DeployFailure(coordinator.exit || 9, "deployment interrupted while retaining the prior release");
  }
  if (coordinator.exit !== 0) {
    failDeploy(coordinator.exit || 5, `cannot prove the coordinator release before prior worker cleanup; deployment journal retained\n${coordinator.stdout}\n${coordinator.stderr}`);
  }
  const reportedCoordinatorPath = coordinator.stdout.trim();
  const coordinatorPath = reportedCoordinatorPath === "absent" ? null : reportedCoordinatorPath;
  if (coordinatorPath !== null
    && (!posix.isAbsolute(coordinatorPath) || /[\r\n\0]/.test(coordinatorPath))) {
    failDeploy(5, "coordinator WorkingDirectory is malformed; deployment journal retained");
  }
  if (!shouldRemovePriorWorkerRelease(prior, journal.targetReleasePath, coordinatorPath, home)) return;
  const removed = await deploySsh(_linuxRemoveManagedWorkerReleaseCommand(prior, home));
  if (signal.aborted) {
    const reason = signal.reason;
    throw reason instanceof DeployFailure
      ? reason
      : new DeployFailure(removed.exit || 9, "deployment interrupted while removing the prior release");
  }
  if (removed.exit !== 0) {
    failDeploy(removed.exit || 5, `cannot retire prior worker release ${prior}; deployment journal retained\n${removed.stdout}\n${removed.stderr}`);
  }
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
    if (journal.phase !== "rolling-back") await remote.checkpointRollback(journal);
    await remote.stopWorker(journal);
    await remote.restorePrior(journal);
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
    await remote.provePrior(journal);
    await remote.removeTarget(journal);
    await remote.clearJournal();
    return { kind: "prior-restored", journal: { ...journal, phase: "rolling-back" } };
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
