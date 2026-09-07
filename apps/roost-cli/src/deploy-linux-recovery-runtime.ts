// Production SSH adapters for Linux worker journal recovery.
// The pure recovery state machine owns ordering; this module translates each
// durable phase, service mutation, and proof into remote commands.

import {
  failDeploy,
  workerServiceIsRunning,
} from "./deploy-exec.ts";
import {
  clearLinuxDeployJournal,
  loadLinuxDeployJournal,
  proveLinuxTargetRelease,
  removeManagedLinuxWorkerRelease,
  _recoverLinuxDeployJournal,
  type ApplyLinuxKeeperUpdate,
  type LinuxDeploySsh,
  type LinuxRecoveryOutcome,
  type ProveLinuxKeeperUpdate,
} from "./deploy-linux-recovery.ts";
import {
  proveLinuxPriorService,
  removePriorLinuxWorkerRelease,
} from "./linux-prior-service-recovery.ts";
import {
  _linuxCheckpointDeployJournalCommand,
  _linuxStartWorkerServiceCommand,
  _linuxStopWorkerServiceCommand,
  _linuxWorkerShaProofCommand,
} from "./linux-deploy-journal-commands.ts";
import {
  _linuxRestorePriorServiceCommand,
  _linuxSettlePriorServiceCommand,
} from "./linux-prior-service-commands.ts";
import type { WorkerRolloutDirective } from "./worker-deploy-rollout.ts";

export async function recoverLinuxDeployJournal(
  deploySsh: LinuxDeploySsh,
  journalPath: string,
  unitPath: string,
  home: string,
  signal: AbortSignal,
  applyKeeperUpdate: ApplyLinuxKeeperUpdate,
  proveKeeperUpdate: ProveLinuxKeeperUpdate,
  directive?: Readonly<WorkerRolloutDirective>,
): Promise<LinuxRecoveryOutcome> {
  return await _recoverLinuxDeployJournal({
    home,
    loadJournal: () => loadLinuxDeployJournal(deploySsh, journalPath, home),
    proveTarget: journal => proveLinuxTargetRelease(deploySsh, journal, home),
    checkpointRollback: async journal => {
      const checkpoint = await deploySsh(
        _linuxCheckpointDeployJournalCommand(journalPath, journal.phase, "rolling-back"),
      );
      if (checkpoint.exit !== 0) {
        failDeploy(checkpoint.exit || 5, "cannot durably choose Linux worker rollback");
      }
      // Re-read: the checkpoint also recorded the target's durable-state
      // version, which decides whether this rollback can ever succeed.
      const refreshed = await loadLinuxDeployJournal(deploySsh, journalPath, home);
      if (refreshed === null) {
        failDeploy(5, "Linux deploy journal disappeared during rollback checkpoint");
      }
      return refreshed;
    },
    checkpointCommit: async journal => {
      const checkpoint = await deploySsh(
        _linuxCheckpointDeployJournalCommand(journalPath, journal.phase, "committing"),
      );
      if (checkpoint.exit !== 0) {
        failDeploy(checkpoint.exit || 5, "cannot durably choose Linux target commit");
      }
    },
    stopWorker: async () => {
      const stopped = await deploySsh(_linuxStopWorkerServiceCommand(journalPath));
      if (stopped.exit !== 0) {
        failDeploy(stopped.exit || 5, "cannot stop the Linux worker; deployment journal retained");
      }
    },
    startWorker: async () => {
      const started = await deploySsh(_linuxStartWorkerServiceCommand(journalPath));
      if (started.exit !== 0) {
        failDeploy(started.exit || 5, "cannot start the Linux worker; deployment journal retained");
      }
    },
    applyKeeperUpdate,
    proveKeeperUpdate,
    restorePrior: async journal => {
      const restored = await deploySsh(
        _linuxRestorePriorServiceCommand(journal, journalPath, unitPath, home),
      );
      if (restored.exit !== 0) {
        failDeploy(restored.exit || 5, "rollback could not restore and start the prior Linux unit");
      }
    },
    settlePrior: async journal => {
      const settled = await deploySsh(
        _linuxSettlePriorServiceCommand(journal, journalPath, home),
      );
      if (settled.exit !== 0) {
        failDeploy(settled.exit || 5, "rollback could not restore the prior Linux lifecycle");
      }
    },
    provePriorWorker: async (_journal, expectedSha) => {
      const proof = await deploySsh(_linuxWorkerShaProofCommand(expectedSha));
      if (proof.exit !== 0 || !workerServiceIsRunning(proof.stdout, "linux")
        || !/^RoostGitShaMatch=yes$/m.test(proof.stdout)) {
        failDeploy(proof.exit || 5, "rollback could not prove the restarted prior Linux worker");
      }
    },
    provePrior: (journal, priorStarted) =>
      proveLinuxPriorService(deploySsh, journal, journalPath, unitPath, home, priorStarted),
    cleanupPrior: journal => removePriorLinuxWorkerRelease(deploySsh, journal, home, signal),
    removeTarget: journal =>
      removeManagedLinuxWorkerRelease(deploySsh, journal.targetReleasePath, home),
    clearJournal: () => clearLinuxDeployJournal(deploySsh, journalPath),
  }, directive);
}
