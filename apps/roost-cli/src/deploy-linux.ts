// `roost deploy <linux-host>` — update an in-place git checkout instead of
// rsyncing a slim tree. A Linux worker is enrolled by join.sh, which clones
// the repo (${ROOST_DIR:-$HOME/Roost}; /srv/roost is the other layout in the
// wild) and pins it to the coordinator's sha, so the box already has the full
// source + .git. Updating it is: fetch, checkout the local HEAD sha, bun
// install, re-run install.sh, verify the systemd unit.
//
// No `tailscale cert` step: the worker has had no inbound TLS surface since
// phase-25e, and no rsync: the checkout is the source of truth.

import { posix } from "node:path";
import {
  parsePosixServiceEnvironment,
  parseSystemdServiceDirective,
} from "./deploy-plist-env.ts";
import {
  acquireRemoteDeployLock,
  DeployFailure,
  failDeploy,
  finishWorkerDeploy,
  POSIX_WORKER_DEPLOY_JOURNAL_PATHS,
  releaseRemoteDeployLock,
  workerServiceIsRunning,
  sshExec,
} from "./deploy-exec.ts";
import { WORKER_UNIT } from "./service-ctl.ts";
import { posixShellQuote } from "@roost/shared/shell-quote";
import {
  isManagedLinuxWorkerReleasePath,
  linuxDeployJournalPath,
  linuxWorkerReleaseRoot,
  malformedLinuxJournal,
} from "./linux-deploy-journal.ts";
import type { LinuxDeployJournal } from "./linux-deploy-journal.ts";
import {
  _linuxCheckpointDeployJournalCommand,
  _linuxClearDeployJournalCommand,
  _linuxPrepareDeployJournalCommand,
  _linuxWorkerShaProofCommand,
} from "./linux-deploy-journal-commands.ts";
import {
  loadLinuxDeployJournal,
  proveLinuxTargetRelease,
  recoverLinuxDeployJournal,
  removeManagedLinuxWorkerRelease,
} from "./deploy-linux-recovery.ts";
import type { LinuxDeploySsh as DeploySsh, LinuxRecoveryOutcome } from "./deploy-linux-recovery.ts";
import { POSIX_FULL_GIT_SHA_RE } from "./posix-deploy-journal.ts";
import { assertWorkerRolloutDirective } from "./worker-deploy-rollout.ts";
import type { WorkerRolloutDirective } from "./worker-deploy-rollout.ts";




export function linuxWorkerResourceEnvironment(definition: string): Record<string, string> {
  const installed = parsePosixServiceEnvironment(definition, "linux");
  const environment: Record<string, string> = {};
  for (const key of [
    "ROOST_WORKER_MEMORY_HIGH",
    "ROOST_WORKER_TASKS_MAX",
    "ROOST_WORKER_LOGROTATE_CONF",
  ] as const) {
    if (installed[key]) environment[key] = installed[key];
  }
  for (const [directive, key] of [
    ["MemoryHigh", "ROOST_WORKER_MEMORY_HIGH"],
    ["TasksMax", "ROOST_WORKER_TASKS_MAX"],
  ] as const) {
    const value = parseSystemdServiceDirective(definition, directive);
    if (value) environment[key] = value;
  }
  return environment;
}


export async function deployLinux(
  host: string,
  opts: {
    gitSha: string;
    passthroughEnv: string;
    machineTransactionPath: string;
    rollout?: WorkerRolloutDirective;
  },
): Promise<void> {
  const { gitSha, passthroughEnv, machineTransactionPath } = opts;
  const rollout = opts.rollout ? assertWorkerRolloutDirective(opts.rollout) : null;
  if (rollout && rollout.targetSha !== gitSha.toLowerCase()) {
    failDeploy(7, "worker rollout target does not match the Linux deployment SHA");
  }

  // The caller has refreshed and proved the source upstream before acquiring
  // any target lease; retain only the exact clean identity at this boundary.
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

    // A fixed journal is always settled while holding the renewable machine
    // lease and before inspecting or staging the next release.
    const initialRecovery = await recoverLinuxDeployJournal(
      deploySsh,
      journalPath,
      unitPath,
      home,
      deployLease.signal,
      rollout ?? undefined,
    );
    if (initialRecovery.kind === "target-held") {
      if (rollout?.action !== "hold") {
        failDeploy(5, "a fleet-held Linux worker requires its owning rollout");
      }
      if (initialRecovery.journal.phase === "activating") {
        const checkpoint = await deploySsh(
          _linuxCheckpointDeployJournalCommand(journalPath, "activating", "activated"),
        );
        if (checkpoint.exit !== 0) {
          failDeploy(checkpoint.exit || 5, "cannot checkpoint recovered Linux fleet activation");
        }
      }
      finishWorkerDeploy(
        initialRecovery.verification,
        `>> held ${host} v2 worker for fleet rollout ${rollout.rolloutId}`,
        "linux",
      );
      return;
    }
    if (initialRecovery.kind === "none"
      && (rollout?.action === "finalize" || rollout?.action === "rollback")) {
      const expectedSha = rollout.action === "finalize" ? rollout.targetSha : rollout.priorSha;
      const proof = await deploySsh(_linuxWorkerShaProofCommand(expectedSha));
      if (proof.exit !== 0 || !workerServiceIsRunning(proof.stdout, "linux")
        || !/^RoostGitShaMatch=yes$/m.test(proof.stdout)) {
        failDeploy(proof.exit || 5, `Linux worker has no journal and does not prove ${expectedSha}`);
      }
      const settlement = rollout.action === "finalize" ? "finalized" : "rolled back";
      finishWorkerDeploy(proof, `>> Linux worker already ${settlement} on ${host}`, "linux");
      return;
    }
    if (rollout?.action === "finalize") {
      if (initialRecovery.kind !== "target-committed") {
        failDeploy(5, "Linux worker target was not finalized");
      }
      finishWorkerDeploy(
        initialRecovery.verification,
        `>> finalized fleet worker ${host}`,
        "linux",
      );
      return;
    }
    if (rollout?.action === "rollback") {
      if (initialRecovery.kind !== "prior-restored"
        && initialRecovery.kind !== "prepared-cleaned") {
        failDeploy(5, "Linux worker prior state was not restored");
      }
      console.log(`>> rolled back fleet worker ${host}`);
      return;
    }
    if (initialRecovery.kind === "prepared-cleaned") {
      console.log(">> recovered interrupted Linux deploy (discarded prepared stage)");
    } else if (initialRecovery.kind === "target-committed") {
      console.log(">> recovered interrupted Linux deploy (verified activated target)");
    } else if (initialRecovery.kind === "prior-restored") {
      console.log(">> recovered interrupted Linux deploy (restored prior service)");
    }

    // The installed unit is authoritative. Accept both a primary checkout
    // (`.git/`) and a staged linked worktree (`.git` file). This discovery is
    // deliberately after journal recovery so a broken activation cannot
    // prevent the next lease owner from repairing the service.
    let remoteRepo = process.env.ROOST_LINUX_REPO_DIR?.trim() ?? "";
    if (!remoteRepo) {
      const probe = await deploySsh(
        `export XDG_RUNTIME_DIR="\${XDG_RUNTIME_DIR:-/run/user/\$(id -u)}"; ` +
          `unit_dir=$(systemctl --user show ${WORKER_UNIT} --property=WorkingDirectory --value 2>/dev/null || true); ` +
          `for d in "$unit_dir" "$HOME/Roost" /srv/roost; do ` +
          `[ -n "$d" ] && git -C "$d" rev-parse --git-dir >/dev/null 2>&1 && echo "$d" && break; done`,
      );
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
      `set -e; mkdir -p ${posixShellQuote(releaseRoot)}; ` +
        `git -C ${posixShellQuote(remoteRepo)} fetch --quiet origin; ` +
        `git -C ${posixShellQuote(remoteRepo)} worktree add --quiet --force --detach ` +
        `${posixShellQuote(releaseDir)} ${posixShellQuote(gitSha)}`,
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
    }));
    if (prepared.exit !== 0) {
      try {
        const recovered = await recoverLinuxDeployJournal(
          deploySsh,
          journalPath,
          unitPath,
          home,
          deployLease.signal,
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
      || journal.targetReleasePath !== releaseDir) {
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
    const priorDefinition = journal.priorUnit ?? "";
    const preservedResources = linuxWorkerResourceEnvironment(priorDefinition);
    const resourceAssignments = Object.entries(preservedResources)
      .map(([key, value]) => `${key}=${posixShellQuote(value)}`)
      .join(" ");
    const activationEnvironment = [passthroughEnv, resourceAssignments]
      .filter(Boolean)
      .join(" ");

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
    const install = await deploySsh(
      `set -eo pipefail; cd ${posixShellQuote(releaseDir)} && ` +
        `bun install --frozen-lockfile 2>&1 | tail -25`,
    );
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

    console.log(`>> activate staged systemd unit (${WORKER_UNIT}) on ${host}`);
    const installSh = await deploySsh(
      `${activationEnvironment} bash ` +
        `${posixShellQuote(posix.join(releaseDir, "apps/worker/scripts/install.sh"))} install 2>&1`,
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
