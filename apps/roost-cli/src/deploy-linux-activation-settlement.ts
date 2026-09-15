// Settles a Linux worker deploy step that failed after the journal was
// prepared: recover the durable journal, keep a target that independently
// verified itself, and otherwise end the deploy with the recovery outcome.
// deploy-linux.ts binds this once per deploy to that deploy's journal, unit and
// lease; the recovery itself belongs to deploy-linux-recovery-runtime.ts.

import { DeployFailure, failDeploy } from "./deploy-exec.ts";
import { recoverLinuxDeployJournal } from "./deploy-linux-recovery-runtime.ts";
import type {
  ApplyLinuxKeeperUpdate,
  LinuxDeploySsh,
  LinuxRecoveryOutcome,
  ProveLinuxKeeperUpdate,
} from "./deploy-linux-recovery.ts";
import { _linuxCheckpointDeployJournalCommand } from "./linux-deploy-journal-commands.ts";
import type { WorkerRolloutDirective } from "./worker-deploy-rollout.ts";

export interface LinuxStepResult {
  exit: number;
  stdout: string;
  stderr: string;
}

export type LinuxActivationSettlement = (
  summary: string,
  failed: LinuxStepResult,
) => Promise<LinuxStepResult>;

export function createLinuxActivationSettlement(deps: {
  deploySsh: LinuxDeploySsh;
  journalPath: string;
  unitPath: string;
  home: string;
  signal: AbortSignal;
  applyKeeperUpdate: ApplyLinuxKeeperUpdate;
  proveKeeperUpdate: ProveLinuxKeeperUpdate;
  rollout: WorkerRolloutDirective | null;
}): LinuxActivationSettlement {
  const { deploySsh, journalPath, rollout } = deps;
  return async (summary, failed) => {
    let recovered: LinuxRecoveryOutcome;
    try {
      recovered = await recoverLinuxDeployJournal(
        deploySsh,
        journalPath,
        deps.unitPath,
        deps.home,
        deps.signal,
        deps.applyKeeperUpdate,
        deps.proveKeeperUpdate,
        rollout?.action === "hold" ? rollout : undefined,
      );
    } catch (recoveryError) {
      const detail = recoveryError instanceof Error
        ? recoveryError.message
        : String(recoveryError);
      const interrupted = deps.signal.reason;
      failDeploy(
        interrupted instanceof DeployFailure
          ? interrupted.exitCode
          : recoveryError instanceof DeployFailure
            ? recoveryError.exitCode
            : failed.exit || 5,
        `${summary}\n${failed.stdout}\n${failed.stderr}\n` +
          `automatic recovery is incomplete; fixed journal retained\n${detail}`,
      );
    }
    if ((recovered.kind === "target-committed" || recovered.kind === "target-held")
      && recovered.verification) {
      if (recovered.kind === "target-held" && recovered.journal.phase === "activating") {
        const checkpoint = await deploySsh(
          _linuxCheckpointDeployJournalCommand(journalPath, "activating", "activated"),
        );
        if (checkpoint.exit !== 0) failDeploy(checkpoint.exit || 5, "cannot checkpoint held Linux target");
      }
      console.warn(`   ${summary}; retained the independently verified target`);
      return recovered.verification;
    }
    const recoveryDetail = recovered.kind === "prior-restored"
      ? "prior worker unit and lifecycle restored"
      : recovered.kind === "prepared-cleaned"
        ? "prepared worker stage removed"
        : recovered.kind === "roll-forward-required"
          ? recovered.reason
          : "no recoverable journal was found";
    failDeploy(
      failed.exit || 5,
      `${summary}\n${failed.stdout}\n${failed.stderr}\n${recoveryDetail}`,
    );
  };
}
