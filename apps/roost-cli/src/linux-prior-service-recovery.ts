// The PRIOR Linux worker release during a rollback: which release may be
// retired, the coordinator probe that protects a shared checkout, and the ssh
// drivers that prove the restored unit or retire the old release. Command
// strings come from linux-prior-service-commands.ts; the recovery state
// machine in deploy-linux-recovery.ts decides when these run.

import { posix } from "node:path";
import {
  DeployFailure, failDeploy, workerServiceIsRunning,
} from "./deploy-exec.ts";
import { parseSystemdServiceDirective } from "./deploy-plist-env.ts";
import {
  DurableStateRollForwardRequired,
  durableStateMigratedForward,
} from "./durable-worker-state.ts";
import { isManagedLinuxWorkerReleasePath } from "./linux-deploy-journal.ts";
import type { LinuxDeployJournal } from "./linux-deploy-journal.ts";
import { _linuxRemoveManagedWorkerReleaseCommand } from "./linux-deploy-journal-commands.ts";
import { _linuxPriorServiceProofCommand } from "./linux-prior-service-commands.ts";
import { COORD_UNIT } from "./service-ctl.ts";
import type { LinuxDeploySsh } from "./deploy-linux-recovery.ts";

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

/**
 * Proves the restored prior unit matches byte-exactly and reached its
 * recorded lifecycle. `priorStarted` states whether this rollback actually
 * started that unit: only a release that was started AND is provably locked
 * out of its durable state may resolve to a roll-forward, so an un-started or
 * transiently failing rollback still retains the journal.
 */
export async function proveLinuxPriorService(
  deploySsh: LinuxDeploySsh,
  journal: LinuxDeployJournal,
  journalPath: string,
  unitPath: string,
  home: string,
  priorStarted: boolean,
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
  if (priorStarted
    && durableStateMigratedForward(
      journal.priorDurableStateVersion,
      journal.targetDurableStateVersion,
    )) {
    throw new DurableStateRollForwardRequired(
      "Linux",
      journal.priorDurableStateVersion!,
      journal.targetDurableStateVersion!,
    );
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
