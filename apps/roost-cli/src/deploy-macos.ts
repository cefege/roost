// macOS worker source deployment: stage an immutable rsynced release, snapshot
// launchd state, activate, and either commit it or hold it for atomic fleet
// settlement. Explicit finalize/rollback uses the companion rollout driver.

import { existsSync, mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, posix } from "node:path";
import { posixShellQuote } from "@roost/shared/shell-quote";
import {
  acquireRemoteDeployLock,
  DeployFailure,
  failDeploy,
  finishWorkerDeploy,
  POSIX_WORKER_DEPLOY_JOURNAL_PATHS,
  releaseRemoteDeployLock,
  remoteMachineTransactionPath,
  RSYNC_RSH,
  run,
  runOrDie,
  sshExec,
} from "./deploy-exec.ts";
import { _backfillEnvFromPlist, _resolveDeployEnvValue, parsePosixServiceEnvironment } from "./deploy-plist-env.ts";
import { manifestOnlyWorkspaces } from "./deploy-workspaces.ts";
import { workerInstallEnvironment } from "./deploy-worker-environment.ts";
import {
  MACOS_WORKER_LABEL,
  _macosDeployJournalPath,
  _recoverMacosDeployJournal,
} from "./deploy-macos-journal.ts";
import type { MacosDeployRecoveryResult } from "./deploy-macos-journal.ts";
import { createMacosDeployJournalController } from "./deploy-macos-journal-controller.ts";
import { settleMacosWorkerRollout } from "./deploy-macos-rollout.ts";
import { assertWorkerRolloutDirective } from "./worker-deploy-rollout.ts";
import type { WorkerRolloutDirective } from "./worker-deploy-rollout.ts";

const REMOTE_DIR = "~/RoostWorkerV2";

export interface MacosDeployOptions {
  sourceCheckout: string;
  gitSha: string;
  coordinatorUrl?: string;
  rollout?: WorkerRolloutDirective;
}

export async function deployMacosWorker(host: string, options: MacosDeployOptions): Promise<void> {
  const rollout = options.rollout ? assertWorkerRolloutDirective(options.rollout) : null;
  const localGitSha = options.gitSha;
  if (rollout && rollout.targetSha !== localGitSha.toLowerCase()) {
    failDeploy(7, "worker rollout target does not match the macOS deployment SHA");
  }
  const { env: hostEnv, filled } = await _backfillEnvFromPlist(host);
  if (filled.length > 0) console.log(`>> reused from existing plist on ${host}: ${filled.join(", ")}`);
  const deployLock = remoteMachineTransactionPath("darwin", hostEnv);
  if (rollout && rollout.action !== "hold") {
    await settleMacosWorkerRollout(host, deployLock, rollout);
    return;
  }
  const resolved = (key: string, invocationValue?: string): string | undefined =>
    _resolveDeployEnvValue(key, hostEnv, invocationValue);
  const resolvedCoordinatorUrl = resolved("ROOST_COORDINATOR_URL", options.coordinatorUrl);
  if (!resolvedCoordinatorUrl) {
    failDeploy(6, `ROOST_COORDINATOR_URL env var required (no prior plist on target to reuse). Set it before running: roost deploy ${host}`);
  }
  if (localGitSha.endsWith("-dirty")) failDeploy(7, "a macOS deploy requires a clean committed source snapshot");

  const releaseId = `${localGitSha}-${crypto.randomUUID()}`;
  const remoteDir = `${REMOTE_DIR}-releases/${releaseId}`;
  const snapshotParent = mkdtempSync(join(tmpdir(), "roost-deploy-source-"));
  const sourceRoot = join(snapshotParent, "source");
  try {
    await runOrDie(
      ["git", "-C", options.sourceCheckout, "worktree", "add", "--quiet", "--force", "--detach", sourceRoot, localGitSha],
      "local source snapshot",
    );
  } catch (error) {
    rmSync(snapshotParent, { recursive: true, force: true });
    throw error;
  }
  const cleanupSource = async (): Promise<void> => {
    await run(["git", "-C", options.sourceCheckout, "worktree", "remove", "--force", sourceRoot], { quiet: true });
    rmSync(snapshotParent, { recursive: true, force: true });
  };

  try {
    const deployLease = await acquireRemoteDeployLock(host, deployLock, releaseId);
    const deploySsh = (command: string) => sshExec(host, command, deployLease.signal);
    try {
      const deployLockBase = posix.dirname(posix.normalize(deployLock));
      const foreignJournalPaths = [
        POSIX_WORKER_DEPLOY_JOURNAL_PATHS.local,
        POSIX_WORKER_DEPLOY_JOURNAL_PATHS.linux,
        POSIX_WORKER_DEPLOY_JOURNAL_PATHS.coordinator,
      ].map(posixShellQuote).join(" ");
      const foreignJournalGuard = await deploySsh(
        `set -e; base_spec=${posixShellQuote(deployLockBase)}; ` +
          `case "$base_spec" in /*) base="$base_spec";; *) base="$HOME/$base_spec";; esac; ` +
          `for relative in ${foreignJournalPaths}; do ` +
          `foreign="$base/$relative"; if test -e "$foreign" || test -L "$foreign"; then exit 66; fi; done`,
      );
      if (foreignJournalGuard.exit !== 0) {
        failDeploy(foreignJournalGuard.exit || 5, `cannot mutate past an unsettled foreign worker deploy journal on ${host}`);
      }
      const journalController = createMacosDeployJournalController(
        deploySsh,
        _macosDeployJournalPath(deployLock),
        deployLease.signal,
      );
      const interrupted = await _recoverMacosDeployJournal(
        journalController.recovery,
        rollout ?? undefined,
      );
      if (interrupted.outcome === "held") {
        finishWorkerDeploy(
          interrupted.targetProof.result,
          `>> held ${host} v2 worker for fleet rollout ${rollout!.rolloutId}`,
          "darwin",
        );
        return;
      }
      if (interrupted.outcome === "prepared-cleaned") console.log(">> cleaned an interrupted prepared macOS release");
      else if (interrupted.outcome === "rolled-back") console.log(">> restored the prior macOS worker from an interrupted activation");
      else if (interrupted.outcome === "committed") console.log(">> committed a previously activated healthy macOS worker");

      console.log(`>> ensure staged release ${remoteDir}/ on ${host}`);
      const manifestOnly = manifestOnlyWorkspaces(sourceRoot);
      const stage = await deploySsh(`mkdir -p ${manifestOnly.map((workspace) => `${remoteDir}/${workspace}`).join(" ")}`);
      if (stage.exit !== 0) failDeploy(stage.exit || 4, `cannot create macOS release stage\n${stage.stdout}\n${stage.stderr}`);
      const cleanupStage = async (): Promise<void> => { await deploySsh(`rm -rf ${remoteDir}`); };

      console.log(`>> rsync canonical workspace + apps/{worker,shared,coord,web}/ + vendor/ → ${host}:${remoteDir}/`);
      const rsyncs = await Promise.allSettled([
        runOrDie(["rsync", "-az", "-e", RSYNC_RSH, "--delete", "--exclude", "node_modules", "--exclude", "tests", "--exclude", "test-results", `${sourceRoot}/apps/worker/`, `${host}:${remoteDir}/apps/worker/`], "rsync apps/worker", deployLease.signal),
        runOrDie(["rsync", "-az", "-e", RSYNC_RSH, "--delete", "--exclude", "node_modules", `${sourceRoot}/apps/shared/`, `${host}:${remoteDir}/apps/shared/`], "rsync apps/shared", deployLease.signal),
        runOrDie(["rsync", "-az", "-e", RSYNC_RSH, "--delete", "--exclude", "node_modules", "--exclude", "tests", "--exclude", "test-results", `${sourceRoot}/apps/coord/`, `${host}:${remoteDir}/apps/coord/`], "rsync apps/coord", deployLease.signal),
        runOrDie(["rsync", "-az", "-e", RSYNC_RSH, "--delete", "--exclude", "node_modules", "--exclude", "dist", "--exclude", "tests", `${sourceRoot}/apps/web/`, `${host}:${remoteDir}/apps/web/`], "rsync apps/web", deployLease.signal),
        ...(existsSync(join(sourceRoot, "vendor"))
          ? [runOrDie(["rsync", "-az", "-e", RSYNC_RSH, "--delete", "--exclude", "node_modules", "--exclude", "dist", `${sourceRoot}/vendor/`, `${host}:${remoteDir}/vendor/`], "rsync vendor", deployLease.signal)]
          : []),
        runOrDie(["rsync", "-az", "-e", RSYNC_RSH, `${sourceRoot}/package.json`, `${sourceRoot}/bun.lock`, `${sourceRoot}/tsconfig.base.json`, `${sourceRoot}/bunfig.toml`, `${host}:${remoteDir}/`], "rsync root workspace", deployLease.signal),
        ...manifestOnly.map((workspace) => runOrDie(["rsync", "-az", "-e", RSYNC_RSH, `${sourceRoot}/${workspace}/package.json`, `${host}:${remoteDir}/${workspace}/`], `rsync ${workspace} manifest`, deployLease.signal)),
      ]);
      const rejectedRsync = rsyncs.find((result): result is PromiseRejectedResult => result.status === "rejected");
      if (rejectedRsync) {
        await cleanupStage();
        throw rejectedRsync.reason;
      }

      console.log(`>> bun install on ${host}`);
      const installRes = await deploySsh(`set -eo pipefail; cd ${remoteDir} && bun install --frozen-lockfile 2>&1 | tail -25`);
      if (installRes.exit !== 0) {
        await cleanupStage();
        failDeploy(4, `bun install failed\n${installRes.stdout}\n${installRes.stderr}`);
      }
      console.log("   bun install ok");
      const passthroughEnv = workerInstallEnvironment(hostEnv, {
        ROOST_COORDINATOR_URL: resolvedCoordinatorUrl,
        ROOST_WORKER_LABEL: resolved("ROOST_WORKER_LABEL"),
        ROOST_BOOTSTRAP_TOKEN: process.env.ROOST_BOOTSTRAP_TOKEN,
        ROOST_REACHABLE_ADDR: resolved("ROOST_REACHABLE_ADDR"),
      }, localGitSha);
      const preparedJournal = await journalController.prepare(localGitSha, remoteDir, rollout?.rolloutId);
      if (rollout) {
        const priorEnvironment = preparedJournal.priorPlistBase64 === null
          ? {}
          : parsePosixServiceEnvironment(Buffer.from(preparedJournal.priorPlistBase64, "base64").toString("utf8"), "darwin");
        const priorSha = priorEnvironment.GIT_SHA ?? priorEnvironment.ROOST_GIT_SHA;
        if (preparedJournal.priorLifecycle !== "running" || priorSha?.toLowerCase() !== rollout.priorSha) {
          await journalController.recovery.removeTarget(preparedJournal);
          await journalController.recovery.clear(preparedJournal);
          failDeploy(5, `macOS worker does not match rollout prior SHA ${rollout.priorSha}`);
        }
      }
      await journalController.checkpointActivating(localGitSha, remoteDir, rollout?.rolloutId);

      const throwIfActivationTransportLost = (
        result: { exit: number; stdout: string; stderr: string },
        operation: string,
      ): void => {
        if (!deployLease.signal.aborted && result.exit !== 255 && result.exit < 128) return;
        const reason = deployLease.signal.reason;
        if (reason instanceof DeployFailure) throw reason;
        throw new DeployFailure(result.exit || 9, `${operation} lost its remote shell; durable macOS deploy journal retained\n${result.stdout}\n${result.stderr}`);
      };
      const recoverFailedActivation = async (exitCode: number, failure: string): Promise<void> => {
        let recovery: MacosDeployRecoveryResult;
        try {
          recovery = await _recoverMacosDeployJournal(journalController.recovery, rollout ?? undefined);
        } catch (error) {
          if (error instanceof DeployFailure
            && (deployLease.signal.aborted || error.exitCode === 255 || error.exitCode >= 128)) throw error;
          failDeploy(exitCode, `${failure}\nrollback failed; durable macOS deploy journal retained\n${error instanceof Error ? error.message : String(error)}`);
        }
        if (recovery.outcome === "committed" || recovery.outcome === "held") {
          finishWorkerDeploy(
            recovery.targetProof.result,
            recovery.outcome === "held"
              ? `>> held ${host} v2 worker for fleet rollout ${rollout!.rolloutId}`
              : `>> done — ${host} v2 worker deployed`,
            "darwin",
          );
          return;
        }
        if (recovery.outcome === "rolled-back") failDeploy(exitCode, `${failure}\nprior worker service restored`);
        failDeploy(exitCode, `${failure}\nmacOS deploy journal disappeared before recovery`);
      };

      const installSh = await deploySsh(`${passthroughEnv} bash ${remoteDir}/apps/worker/scripts/install.sh install 2>&1`);
      if (installSh.exit !== 0) {
        throwIfActivationTransportLost(installSh, "install macOS worker");
        await recoverFailedActivation(5, `install.sh failed\n${installSh.stdout}\n${installSh.stderr}`);
        return;
      }
      const installOutput = installSh.stdout.trim();
      if (installOutput) console.log(installOutput.split("\n").map((line) => `   ${line}`).join("\n"));

      console.log(`>> kickstart ${MACOS_WORKER_LABEL} on ${host}`);
      const kick = await deploySsh(`launchctl kickstart -k gui/$(id -u)/${MACOS_WORKER_LABEL} 2>&1`);
      if (kick.exit !== 0) {
        throwIfActivationTransportLost(kick, "kickstart macOS worker");
        await recoverFailedActivation(4, `kickstart failed (exit ${kick.exit})\n${kick.stdout}\n${kick.stderr}`);
        return;
      }
      console.log(`>> verifying service is up on ${host}`);
      await new Promise<void>((resolve) => setTimeout(resolve, 1500));
      const activation = await _recoverMacosDeployJournal(journalController.recovery, rollout ?? undefined);
      if (activation.outcome === "committed" || activation.outcome === "held") {
        finishWorkerDeploy(
          activation.targetProof.result,
          activation.outcome === "held"
            ? `>> held ${host} v2 worker for fleet rollout ${rollout!.rolloutId}`
            : `>> done — ${host} v2 worker deployed`,
          "darwin",
        );
      } else if (activation.outcome === "rolled-back") {
        const proof = activation.targetProof?.result ?? { exit: 8, stdout: "", stderr: "target proof unavailable" };
        failDeploy(proof.exit || 8, `worker service verification failed\n${proof.stdout}\n${proof.stderr}\nprior worker service restored`);
      } else {
        failDeploy(8, "macOS activation journal was not available for final verification");
      }
    } finally {
      await releaseRemoteDeployLock(host, deployLock, releaseId);
    }
  } finally {
    await cleanupSource();
  }
}
