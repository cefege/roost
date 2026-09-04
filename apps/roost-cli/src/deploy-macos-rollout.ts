// Explicit settlement driver for a macOS worker held by an atomic fleet
// rollout. It reacquires the host lease, binds every action to the journaled
// rollout, and proves an already-cleared action by exact running Git SHA.

import { posixShellQuote } from "@roost/shared/shell-quote";
import {
  acquireRemoteDeployLock,
  DeployFailure,
  failDeploy,
  finishWorkerDeploy,
  releaseRemoteDeployLock,
  sshExec,
  workerServiceIsRunning,
} from "./deploy-exec.ts";
import {
  _macosDeployJournalPath,
  _recoverMacosDeployJournal,
  MACOS_WORKER_LABEL,
} from "./deploy-macos-journal.ts";
import { createMacosDeployJournalController } from "./deploy-macos-journal-controller.ts";
import { verifyWorkerCmd } from "./service-ctl.ts";
import { assertWorkerRolloutDirective } from "./worker-deploy-rollout.ts";
import type { WorkerRolloutDirective } from "./worker-deploy-rollout.ts";

const MACOS_WORKER_PLIST = `Library/LaunchAgents/${MACOS_WORKER_LABEL}.plist`;

function macosWorkerShaProofCommand(expectedSha: string): string {
  return `${verifyWorkerCmd("darwin")}; verify_status=$?; ` +
    `sha=$(/usr/libexec/PlistBuddy -c 'Print :EnvironmentVariables:GIT_SHA' ` +
    `"$HOME/${MACOS_WORKER_PLIST}" 2>/dev/null || true); ` +
    `if test "$verify_status" -eq 0 && test "$sha" = ${posixShellQuote(expectedSha)}; ` +
    `then echo RoostGitShaMatch=yes; else exit 1; fi`;
}

export async function settleMacosWorkerRollout(
  host: string,
  machineTransactionPath: string,
  rawDirective: Readonly<WorkerRolloutDirective>,
): Promise<void> {
  const directive = assertWorkerRolloutDirective(rawDirective);
  if (directive.action === "hold") {
    throw new Error("macOS hold settlement is performed by the deployment driver");
  }
  const leaseOwner = `${directive.rolloutId}-${crypto.randomUUID()}`;
  const lease = await acquireRemoteDeployLock(host, machineTransactionPath, leaseOwner);
  const execute = (command: string) => sshExec(host, command, lease.signal);
  try {
    const journalPath = _macosDeployJournalPath(machineTransactionPath);
    const controller = createMacosDeployJournalController(execute, journalPath, lease.signal);
    const result = await _recoverMacosDeployJournal(controller.recovery, directive);
    if (result.outcome === "none") {
      const expectedSha = directive.action === "finalize" ? directive.targetSha : directive.priorSha;
      const proof = await execute(macosWorkerShaProofCommand(expectedSha));
      if (proof.exit !== 0 || !workerServiceIsRunning(proof.stdout, "darwin")
        || !/^RoostGitShaMatch=yes$/m.test(proof.stdout)) {
        failDeploy(proof.exit || 5, `macOS worker has no journal and does not prove ${expectedSha}`);
      }
      finishWorkerDeploy(
        proof,
        `>> macOS worker already ${directive.action === "finalize" ? "finalized" : "rolled back"} on ${host}`,
        "darwin",
      );
      return;
    }
    if (directive.action === "finalize") {
      if (result.outcome !== "committed") {
        throw new DeployFailure(5, "macOS worker target was not finalized");
      }
      finishWorkerDeploy(result.targetProof.result, `>> finalized fleet worker ${host}`, "darwin");
      return;
    }
    if (result.outcome !== "rolled-back" && result.outcome !== "prepared-cleaned") {
      throw new DeployFailure(5, "macOS worker prior state was not restored");
    }
    console.log(`>> rolled back fleet worker ${host}`);
  } finally {
    await releaseRemoteDeployLock(host, machineTransactionPath, leaseOwner);
  }
}
