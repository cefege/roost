// Linux worker journal recovery and explicit fleet settlement. The source
// deploy driver supplies the leased SSH transport; this module owns exact
// target/prior proof, confined cleanup, and hold/finalize/rollback decisions.

import { posix } from "node:path";
import {
  parsePosixServiceEnvironment,
  parseSystemdServiceDirective,
} from "./deploy-plist-env.ts";
import {
  DeployFailure,
  failDeploy,
  workerServiceIsRunning,
  workerServiceMatchesRelease,
} from "./deploy-exec.ts";
import { COORD_UNIT } from "./service-ctl.ts";
import {
  isManagedLinuxWorkerReleasePath,
  linuxDeployRecoveryPlan,
  parseLinuxDeployJournalSnapshot,
} from "./linux-deploy-journal.ts";
import type { LinuxDeployJournal } from "./linux-deploy-journal.ts";
import {
  _linuxClearDeployJournalCommand,
  _linuxLoadDeployJournalCommand,
  _linuxPriorServiceProofCommand,
  _linuxRemoveManagedWorkerReleaseCommand,
  _linuxRestorePriorServiceCommand,
  _linuxTargetVerificationCommand,
} from "./linux-deploy-journal-commands.ts";
import {
  assertWorkerRolloutDirective,
  assertWorkerRolloutMatches,
} from "./worker-deploy-rollout.ts";
import type { WorkerRolloutDirective } from "./worker-deploy-rollout.ts";

export type LinuxDeploySsh = (
  command: string,
) => Promise<{ exit: number; stdout: string; stderr: string }>;

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
  restorePrior: (journal: LinuxDeployJournal) => Promise<void>;
  provePrior: (journal: LinuxDeployJournal) => Promise<void>;
  cleanupPrior: (journal: LinuxDeployJournal) => Promise<void>;
  removeTarget: (journal: LinuxDeployJournal) => Promise<void>;
  clearJournal: () => Promise<void>;
}

export type LinuxRecoveryOutcome =
  | { kind: "none" }
  | { kind: "prepared-cleaned"; journal: LinuxDeployJournal }
  | { kind: "target-held"; journal: LinuxDeployJournal; verification: { exit: number; stdout: string; stderr: string } }
  | { kind: "target-committed"; journal: LinuxDeployJournal; verification: { exit: number; stdout: string; stderr: string } }
  | { kind: "prior-restored"; journal: LinuxDeployJournal };

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

async function clearLinuxDeployJournal(deploySsh: LinuxDeploySsh, journalPath: string): Promise<void> {
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

async function proveLinuxPriorService(
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

async function removePriorLinuxWorkerRelease(
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
  if (requested) {
    assertWorkerRolloutMatches(journal, requested);
    const priorEnvironment = journal.priorUnit
      ? parsePosixServiceEnvironment(journal.priorUnit, "linux")
      : {};
    const priorSha = priorEnvironment.GIT_SHA ?? priorEnvironment.ROOST_GIT_SHA;
    if (journal.priorLifecycle !== "running" || priorSha?.toLowerCase() !== requested.priorSha) {
      throw new DeployFailure(5, "Linux worker journal does not prove the fleet rollout prior identity");
    }
  }
  if (journal.rolloutId !== null && !requested) {
    throw new DeployFailure(5, "a fleet rollout still owns the Linux worker deploy journal");
  }
  const restorePrior = async (): Promise<LinuxRecoveryOutcome> => {
    await remote.restorePrior(journal);
    await remote.provePrior(journal);
    await remote.removeTarget(journal);
    await remote.clearJournal();
    return { kind: "prior-restored", journal };
  };
  if (journal.phase === "prepared") {
    if (requested?.action === "finalize") {
      throw new DeployFailure(5, "cannot finalize a Linux worker before activation");
    }
    await remote.removeTarget(journal);
    await remote.clearJournal();
    return { kind: "prepared-cleaned", journal };
  }
  if (requested?.action === "rollback") return await restorePrior();
  const target = await remote.proveTarget(journal);
  if (journal.rolloutId !== null) {
    if (requested?.action === "finalize") {
      if (journal.phase !== "activated" || !target.healthy) {
        throw new DeployFailure(5, "cannot finalize an unproven Linux worker target");
      }
      await remote.cleanupPrior(journal);
      await remote.clearJournal();
      return { kind: "target-committed", journal, verification: target.proof };
    }
    if (target.healthy) return { kind: "target-held", journal, verification: target.proof };
    return await restorePrior();
  }
  const plan = linuxDeployRecoveryPlan(journal, target.healthy, remote.home);
  if (plan.kind === "commit-target") {
    await remote.cleanupPrior(journal);
    await remote.clearJournal();
    return { kind: "target-committed", journal, verification: target.proof };
  }
  return await restorePrior();
}

export async function recoverLinuxDeployJournal(
  deploySsh: LinuxDeploySsh,
  journalPath: string,
  unitPath: string,
  home: string,
  signal: AbortSignal,
  directive?: Readonly<WorkerRolloutDirective>,
): Promise<LinuxRecoveryOutcome> {
  return await _recoverLinuxDeployJournal({
    home,
    loadJournal: () => loadLinuxDeployJournal(deploySsh, journalPath, home),
    proveTarget: (journal) => proveLinuxTargetRelease(deploySsh, journal, home),
    restorePrior: async (journal) => {
      const restored = await deploySsh(_linuxRestorePriorServiceCommand(journal, journalPath, unitPath, home));
      if (restored.exit !== 0) {
        failDeploy(restored.exit || 5, `rollback could not restore the prior Linux unit; deployment journal retained\n${restored.stdout}\n${restored.stderr}`);
      }
    },
    provePrior: (journal) => proveLinuxPriorService(deploySsh, journal, journalPath, unitPath, home),
    cleanupPrior: (journal) => removePriorLinuxWorkerRelease(deploySsh, journal, home, signal),
    removeTarget: (journal) => removeManagedLinuxWorkerRelease(deploySsh, journal.targetReleasePath, home),
    clearJournal: () => clearLinuxDeployJournal(deploySsh, journalPath),
  }, directive);
}
