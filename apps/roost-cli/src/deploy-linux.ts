// Linux remote worker deployment stages an exact git release, journals the
// installed systemd unit, and activates it while holding the host lease.
// join.sh owns enrollment; this driver owns update and crash settlement.
import { posix } from "node:path";
import {
  JournaledKeeperUpdateV1Schema,
  type JournaledKeeperUpdateV1,
} from "@roost/shared/keeper-update";
import {
  acquireRemoteDeployLock,
  DeployFailure,
  failDeploy,
  finishWorkerDeploy,
  POSIX_WORKER_DEPLOY_JOURNAL_PATHS,
  releaseRemoteDeployLock,
  sshExec,
} from "./deploy-exec.ts";
import { parsePosixServiceEnvironment } from "./deploy-plist-env.ts";
import { WORKER_UNIT } from "./service-ctl.ts";
import { posixShellQuote } from "@roost/shared/shell-quote";
import {
  isManagedLinuxWorkerReleasePath,
  linuxDeployJournalPath,
  linuxWorkerReleaseRoot,
  malformedLinuxJournal,
  serializeLinuxKeeperUpdate,
} from "./linux-deploy-journal.ts";
import {
  _linuxActivateWorkerReleaseCommand,
  _linuxCheckpointDeployJournalCommand,
  linuxWorkerActivationEnvironment,
  _linuxClearDeployJournalCommand,
  _linuxInstallWorkerDependenciesCommand,
  _linuxPrepareDeployJournalCommand,
  _linuxStopWorkerServiceCommand,
  _linuxStageWorkerReleaseCommand,
  _linuxWorkerCheckoutProbeCommand,
} from "./linux-deploy-journal-commands.ts";
import {
  loadLinuxDeployJournal,
  proveLinuxTargetRelease,
  settleInitialLinuxRecovery,
  removeManagedLinuxWorkerRelease,
} from "./deploy-linux-recovery.ts";
import { recoverLinuxDeployJournal } from "./deploy-linux-recovery-runtime.ts";
import type {
  ApplyLinuxKeeperUpdate, LinuxDeploySsh as DeploySsh, LinuxRecoveryOutcome,
  ProveLinuxKeeperUpdate,
} from "./deploy-linux-recovery.ts";
import { POSIX_FULL_GIT_SHA_RE } from "./posix-deploy-journal.ts";
import {
  assertWorkerRolloutDirective, assertWorkerRolloutMatches,
  type WorkerRolloutDirective,
} from "./worker-deploy-rollout.ts";
import type { DirectKeeperAdmission } from "./direct-keeper-update.ts";

export async function deployLinux(
  host: string,
  opts: {
    gitSha: string;
    passthroughEnv: string;
    machineTransactionPath: string;
    rollout?: WorkerRolloutDirective;
    keeperUpdate: JournaledKeeperUpdateV1 | null;
    workerFingerprint: string | null;
    resolveKeeperAdmission?: () => Promise<DirectKeeperAdmission | null>;
    applyKeeperUpdate: ApplyLinuxKeeperUpdate;
    proveKeeperUpdate: ProveLinuxKeeperUpdate;
  },
): Promise<void> {
  const {
    gitSha,
    passthroughEnv,
    machineTransactionPath,
    applyKeeperUpdate,
    proveKeeperUpdate,
  } = opts;
  let keeperUpdate = opts.keeperUpdate === null
    ? null
    : JournaledKeeperUpdateV1Schema.parse(opts.keeperUpdate);
  let workerFingerprint = opts.workerFingerprint;
  const rollout = opts.rollout ? assertWorkerRolloutDirective(opts.rollout) : null;
  if (rollout && rollout.targetSha !== gitSha.toLowerCase()) {
    failDeploy(7, "worker rollout target does not match the Linux deployment SHA");
  }
  if (rollout && workerFingerprint !== rollout.workerFingerprint) {
    failDeploy(7, "worker fingerprint does not match the Linux rollout directive");
  }
  if (rollout) {
    assertWorkerRolloutMatches({
      rolloutId: rollout.rolloutId,
      workerFingerprint: rollout.workerFingerprint,
      targetSha: gitSha,
      keeperUpdate,
    }, rollout);
  }
  if (!POSIX_FULL_GIT_SHA_RE.test(gitSha) || gitSha.endsWith("-dirty")) {
    failDeploy(7, "a Linux deploy requires a clean pushed commit");
  }
  const releaseId = `${gitSha}-${crypto.randomUUID()}`;
  const deployLease = await acquireRemoteDeployLock(host, machineTransactionPath, releaseId);
  const deploySsh: DeploySsh = (command) => sshExec(host, command, deployLease.signal);
  try {
    const resolvedHome = await deploySsh("set -e; cd ~ && pwd");
    if (resolvedHome.exit !== 0) {
      failDeploy(
        resolvedHome.exit || 2,
        `cannot resolve the remote Linux home directory\n${resolvedHome.stdout}\n${resolvedHome.stderr}`,
      );
    }
    const home = resolvedHome.stdout.trim();
    const releaseRoot = linuxWorkerReleaseRoot(home);
    const journalPath = linuxDeployJournalPath(machineTransactionPath, home);
    const foreignJournalPaths = [
      POSIX_WORKER_DEPLOY_JOURNAL_PATHS.local,
      POSIX_WORKER_DEPLOY_JOURNAL_PATHS.darwin,
      POSIX_WORKER_DEPLOY_JOURNAL_PATHS.coordinator,
    ].map(posixShellQuote).join(" ");
    const foreignJournalGuard = await deploySsh(
      `set -e; base=${posixShellQuote(posix.dirname(journalPath))}; ` +
        `for relative in ${foreignJournalPaths}; do ` +
        `foreign="$base/$relative"; ` +
        `if test -e "$foreign" || test -L "$foreign"; then exit 66; fi; done`,
    );
    if (foreignJournalGuard.exit !== 0) {
      failDeploy(
        foreignJournalGuard.exit || 5,
        `cannot mutate past an unsettled foreign worker deploy journal on ${host}`,
      );
    }
    const unitPath = posix.join(home, ".config", "systemd", "user", WORKER_UNIT);
    const initialRecovery = await recoverLinuxDeployJournal(
      deploySsh,
      journalPath,
      unitPath,
      home,
      deployLease.signal,
      applyKeeperUpdate,
      proveKeeperUpdate,
      rollout ?? undefined,
    );
    if (await settleInitialLinuxRecovery(
      host,
      initialRecovery,
      rollout,
      deploySsh,
      journalPath,
    )) return;
    if (opts.resolveKeeperAdmission) {
      const admission = await opts.resolveKeeperAdmission();
      keeperUpdate = admission?.keeperUpdate ?? null;
      workerFingerprint = admission?.workerFingerprint ?? null;
    }
    if ((keeperUpdate === null) !== (workerFingerprint === null)) {
      failDeploy(7, "Linux keeper update and worker fingerprint must be present together");
    }
    if (keeperUpdate === null) {
      const absentUnit = await deploySsh(
        `test ! -e ${posixShellQuote(unitPath)} && test ! -L ${posixShellQuote(unitPath)}`,
      );
      if (absentUnit.exit !== 0) {
        failDeploy(
          5,
          "existing Linux worker requires keeper update admission before staging",
        );
      }
    }
    let remoteRepo = process.env.ROOST_LINUX_REPO_DIR?.trim() ?? "";
    if (!remoteRepo) {
      const probe = await deploySsh(_linuxWorkerCheckoutProbeCommand());
      remoteRepo = probe.stdout.trim();
    }
    if (!remoteRepo) {
      throw new Error(
        `no worker checkout found on ${host} (checked ${WORKER_UNIT} WorkingDirectory, ~/Roost and /srv/roost) — ` +
          "run join.sh first or set ROOST_LINUX_REPO_DIR",
      );
    }
    if (!posix.isAbsolute(remoteRepo) || /[\r\n\0]/.test(remoteRepo)) {
      failDeploy(2, `worker checkout path from ${host} is unsafe: ${JSON.stringify(remoteRepo)}`);
    }
    const releaseDir = posix.join(releaseRoot, releaseId);
    if (!isManagedLinuxWorkerReleasePath(releaseDir, home)) {
      failDeploy(2, `generated Linux worker release path is unsafe: ${releaseDir}`);
    }
    const cleanupStage = () =>
      removeManagedLinuxWorkerRelease(deploySsh, releaseDir, home);
    console.log(`>> stage ${gitSha.slice(0, 8)} in ${host}:${releaseDir}`);
    const stage = await deploySsh(
      _linuxStageWorkerReleaseCommand(remoteRepo, releaseRoot, releaseDir, gitSha),
    );
    if (stage.exit !== 0) {
      if (!deployLease.signal.aborted) await cleanupStage();
      failDeploy(stage.exit || 2, `git worktree staging failed\n${stage.stdout}\n${stage.stderr}`);
    }
    const prepared = await deploySsh(_linuxPrepareDeployJournalCommand({
      journalPath,
      unitPath,
      targetSha: gitSha,
      targetReleasePath: releaseDir,
      home,
      rolloutId: rollout?.action === "hold" ? rollout.rolloutId : null,
      workerFingerprint,
      keeperUpdate,
    }));
    if (prepared.exit !== 0) {
      try {
        const recovered = await recoverLinuxDeployJournal(
          deploySsh,
          journalPath,
          unitPath,
          home,
          deployLease.signal,
          applyKeeperUpdate,
          proveKeeperUpdate,
          rollout?.action === "hold" ? rollout : undefined,
        );
        if (recovered.kind === "none") await cleanupStage();
      } catch (recoveryError) {
        const detail = recoveryError instanceof Error
          ? recoveryError.message
          : String(recoveryError);
        failDeploy(
          recoveryError instanceof DeployFailure
            ? recoveryError.exitCode
            : prepared.exit || 5,
          `cannot durably prepare the Linux deployment journal; recovery remains pending\n${detail}`,
        );
      }
      failDeploy(
        prepared.exit || 5,
        `cannot durably snapshot ${WORKER_UNIT} before activation\n${prepared.stdout}\n${prepared.stderr}`,
      );
    }
    const journal = await loadLinuxDeployJournal(deploySsh, journalPath, home);
    if (journal === null
      || journal.phase !== "prepared"
      || journal.targetSha !== gitSha
      || journal.workerFingerprint !== workerFingerprint
      || journal.targetReleasePath !== releaseDir
      || serializeLinuxKeeperUpdate(journal.keeperUpdate)
        !== serializeLinuxKeeperUpdate(keeperUpdate)) {
      malformedLinuxJournal("prepared checkpoint does not identify the staged target");
    }
    if (rollout?.action === "hold") {
      const priorEnvironment = journal.priorUnit
        ? parsePosixServiceEnvironment(journal.priorUnit, "linux")
        : {};
      const priorSha = priorEnvironment.GIT_SHA ?? priorEnvironment.ROOST_GIT_SHA;
      if (journal.priorLifecycle !== "running" || priorSha?.toLowerCase() !== rollout.priorSha) {
        await cleanupStage();
        const cleared = await deploySsh(_linuxClearDeployJournalCommand(journalPath));
        if (cleared.exit !== 0) {
          failDeploy(cleared.exit || 5, "cannot clear rejected Linux worker deploy journal");
        }
        failDeploy(5, `Linux worker does not match rollout prior SHA ${rollout.priorSha}`);
      }
    }
    const activationEnvironment = linuxWorkerActivationEnvironment(
      journal.priorUnit ?? "",
      passthroughEnv,
    );
    const settleActivationFailure = async (
      summary: string,
      failed: { exit: number; stdout: string; stderr: string },
    ): Promise<{ exit: number; stdout: string; stderr: string }> => {
      let recovered: LinuxRecoveryOutcome;
      try {
        recovered = await recoverLinuxDeployJournal(
          deploySsh,
          journalPath,
          unitPath,
          home,
          deployLease.signal,
          applyKeeperUpdate,
          proveKeeperUpdate,
          rollout?.action === "hold" ? rollout : undefined,
        );
      } catch (recoveryError) {
        const detail = recoveryError instanceof Error
          ? recoveryError.message
          : String(recoveryError);
        const interrupted = deployLease.signal.reason;
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
          : "no recoverable journal was found";
      failDeploy(
        failed.exit || 5,
        `${summary}\n${failed.stdout}\n${failed.stderr}\n${recoveryDetail}`,
      );
    };
    console.log(`>> frozen bun install on ${host}`);
    const install = await deploySsh(_linuxInstallWorkerDependenciesCommand(releaseDir));
    if (install.exit !== 0) {
      await settleActivationFailure("bun install failed", install);
    }
    console.log("   bun install ok");
    const activating = await deploySsh(
      _linuxCheckpointDeployJournalCommand(journalPath, "prepared", "activating"),
    );
    if (activating.exit !== 0) {
      await settleActivationFailure("cannot checkpoint Linux activation", activating);
    }
    if (journal.keeperUpdate) {
      try {
        await applyKeeperUpdate(
          journal.workerFingerprint!,
          journal.keeperUpdate,
          "target",
          journal.targetReleasePath,
        );
      } catch (error) {
        const keeperFailure = {
          exit: error instanceof DeployFailure ? error.exitCode : 5,
          stdout: "",
          stderr: error instanceof Error ? error.message : String(error),
        };
        await settleActivationFailure("keeper update action failed", keeperFailure);
        throw error;
      }
    }
    const stopped = await deploySsh(_linuxStopWorkerServiceCommand(journalPath));
    if (stopped.exit !== 0) {
      await settleActivationFailure("cannot stop Linux worker for keeper update", stopped);
      failDeploy(
        stopped.exit || 5,
        "Linux worker stop boundary could not be proved after keeper preparation",
      );
    }
    console.log(`>> activate staged systemd unit (${WORKER_UNIT}) on ${host}`);
    const installSh = await deploySsh(
      _linuxActivateWorkerReleaseCommand(releaseDir, activationEnvironment),
    );
    if (installSh.exit !== 0) {
      const committed = await settleActivationFailure("install.sh failed", installSh);
      finishWorkerDeploy(
        committed,
        `>> done — ${host} v2 worker deployed (linux)`,
        "linux",
      );
      return;
    }
    console.log(`>> verifying service is up on ${host}`);
    const target = await proveLinuxTargetRelease(deploySsh, journal, home);
    if (!target.healthy) {
      const committed = await settleActivationFailure(
        "worker service verification failed",
        target.proof,
      );
      finishWorkerDeploy(
        committed,
        `>> done — ${host} v2 worker deployed (linux)`,
        "linux",
      );
      return;
    }
    const activated = await deploySsh(
      _linuxCheckpointDeployJournalCommand(journalPath, "activating", "activated"),
    );
    if (activated.exit !== 0) {
      const committed = await settleActivationFailure(
        "cannot checkpoint verified Linux activation",
        activated,
      );
      finishWorkerDeploy(
        committed,
        `>> done — ${host} v2 worker deployed (linux)`,
        "linux",
      );
      return;
    }
    const settlement = await recoverLinuxDeployJournal(
      deploySsh,
      journalPath,
      unitPath,
      home,
      deployLease.signal,
      applyKeeperUpdate,
      proveKeeperUpdate,
      rollout?.action === "hold" ? rollout : undefined,
    );
    if ((settlement.kind !== "target-held" && settlement.kind !== "target-committed")
      || !settlement.verification) {
      failDeploy(5, "verified Linux worker did not reach its requested settlement state");
    }
    finishWorkerDeploy(
      settlement.verification,
      settlement.kind === "target-held"
        ? `>> held ${host} v2 worker for fleet rollout ${rollout!.rolloutId}`
        : `>> done — ${host} v2 worker deployed (linux)`,
      "linux",
    );
  } finally {
    await releaseRemoteDeployLock(host, machineTransactionPath, releaseId);
  }
}
