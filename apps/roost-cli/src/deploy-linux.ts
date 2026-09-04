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
  workerServiceMatchesRelease,
  sshExec,
} from "./deploy-exec.ts";
import { COORD_UNIT, WORKER_UNIT } from "./service-ctl.ts";
import { posixShellQuote } from "@roost/shared/shell-quote";
import {
  isManagedLinuxWorkerReleasePath,
  linuxDeployJournalPath,
  linuxDeployRecoveryPlan,
  linuxWorkerReleaseRoot,
  malformedLinuxJournal,
  parseLinuxDeployJournalSnapshot,
  type LinuxDeployJournal,
} from "./linux-deploy-journal.ts";
import {
  _linuxCheckpointDeployJournalCommand,
  _linuxClearDeployJournalCommand,
  _linuxLoadDeployJournalCommand,
  _linuxPrepareDeployJournalCommand,
  _linuxPriorServiceProofCommand,
  _linuxRemoveManagedWorkerReleaseCommand,
  _linuxRestorePriorServiceCommand,
  _linuxTargetVerificationCommand,
} from "./linux-deploy-journal-commands.ts";
import { POSIX_FULL_GIT_SHA_RE } from "./posix-deploy-journal.ts";


type DeploySsh = (
  command: string,
) => Promise<{ exit: number; stdout: string; stderr: string }>;



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

export function shouldRemovePriorWorkerRelease(
  prior: string,
  current: string,
  coordinator: string | null,
  home: string,
): boolean {
  if ((coordinator !== null
    && (!posix.isAbsolute(coordinator) || /[\r\n\0]/.test(coordinator)))
    || !prior || prior === current || prior === coordinator
    || !isManagedLinuxWorkerReleasePath(prior, home)
    || !isManagedLinuxWorkerReleasePath(current, home)) {
    return false;
  }
  return true;
}

export function linuxCoordinatorWorkingDirectoryCommand(): string {
  return `set -e; export XDG_RUNTIME_DIR="\${XDG_RUNTIME_DIR:-/run/user/\$(id -u)}"; ` +
    `load_state=$(systemctl --user show ${COORD_UNIT} --property=LoadState --value); ` +
    `case "$load_state" in ` +
    `not-found) printf 'absent\\n';; ` +
    `loaded) systemctl --user show ${COORD_UNIT} --property=WorkingDirectory --value;; ` +
    `*) exit 65;; esac`;
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

export interface LinuxRecoveryOutcome {
  kind: "none" | "prepared-cleaned" | "target-committed" | "prior-restored";
  verification?: { exit: number; stdout: string; stderr: string };
}

async function loadLinuxDeployJournal(
  deploySsh: DeploySsh,
  journalPath: string,
  home: string,
): Promise<LinuxDeployJournal | null> {
  const loaded = await deploySsh(_linuxLoadDeployJournalCommand(journalPath));
  if (loaded.exit !== 0) {
    failDeploy(
      loaded.exit || 5,
      `cannot read the fixed Linux deployment journal; it was left intact\n${loaded.stdout}\n${loaded.stderr}`,
    );
  }
  return parseLinuxDeployJournalSnapshot(loaded.stdout, home);
}

async function removeManagedLinuxWorkerRelease(
  deploySsh: DeploySsh,
  targetReleasePath: string,
  home: string,
): Promise<void> {
  const removed = await deploySsh(
    _linuxRemoveManagedWorkerReleaseCommand(targetReleasePath, home),
  );
  if (removed.exit !== 0) {
    failDeploy(
      removed.exit || 5,
      `cannot remove managed worker stage ${targetReleasePath}; deployment journal retained\n` +
        `${removed.stdout}\n${removed.stderr}`,
    );
  }
}

async function clearLinuxDeployJournal(
  deploySsh: DeploySsh,
  journalPath: string,
): Promise<void> {
  const cleared = await deploySsh(_linuxClearDeployJournalCommand(journalPath));
  if (cleared.exit !== 0) {
    failDeploy(
      cleared.exit || 5,
      `cannot durably clear the Linux deployment journal\n${cleared.stdout}\n${cleared.stderr}`,
    );
  }
}

async function proveLinuxTargetRelease(
  deploySsh: DeploySsh,
  journal: LinuxDeployJournal,
  home: string,
  attempts = 20,
): Promise<{
  healthy: boolean;
  proof: { exit: number; stdout: string; stderr: string };
}> {
  let proof = { exit: 1, stdout: "", stderr: "target verification was not attempted" };
  for (let attempt = 0; attempt < attempts; attempt += 1) {
    proof = await deploySsh(_linuxTargetVerificationCommand(journal, home));
    if (proof.exit === 0
      && workerServiceIsRunning(proof.stdout, "linux")
      && workerServiceMatchesRelease(proof.stdout)) {
      return { healthy: true, proof };
    }
    if (proof.exit === 9 || proof.exit === 130 || proof.exit === 143) break;
    if (attempt + 1 < attempts) await Bun.sleep(250);
  }
  return { healthy: false, proof };
}

async function proveLinuxPriorService(
  deploySsh: DeploySsh,
  journal: LinuxDeployJournal,
  journalPath: string,
  unitPath: string,
  home: string,
): Promise<{ exit: number; stdout: string; stderr: string }> {
  let proof = { exit: 1, stdout: "", stderr: "rollback verification was not attempted" };
  for (let attempt = 0; attempt < 20; attempt += 1) {
    proof = await deploySsh(
      _linuxPriorServiceProofCommand(journal, journalPath, unitPath, home),
    );
    const lifecycleMatches = /^RoostPriorStateMatch=yes$/m.test(proof.stdout);
    const runningMatches = journal.priorLifecycle !== "running"
      || workerServiceIsRunning(proof.stdout, "linux");
    if (proof.exit === 0 && lifecycleMatches && runningMatches) return proof;
    if (proof.exit === 9 || proof.exit === 130 || proof.exit === 143) break;
    if (attempt < 19) await Bun.sleep(250);
  }
  failDeploy(
    proof.exit || 5,
    `rollback could not prove the exact prior unit and lifecycle; deployment journal retained\n` +
      `${proof.stdout}\n${proof.stderr}`,
  );
}

async function removePriorLinuxWorkerRelease(
  deploySsh: DeploySsh,
  journal: LinuxDeployJournal,
  home: string,
  signal: AbortSignal,
): Promise<void> {
  const prior = journal.priorUnit === null
    ? ""
    : parseSystemdServiceDirective(journal.priorUnit, "WorkingDirectory") ?? "";
  if (!prior || prior === journal.targetReleasePath) return;
  if (!shouldRemovePriorWorkerRelease(
    prior,
    journal.targetReleasePath,
    "/dev/null",
    home,
  )) return;
  const coordinator = await deploySsh(linuxCoordinatorWorkingDirectoryCommand());
  if (signal.aborted) {
    const reason = signal.reason;
    throw reason instanceof DeployFailure
      ? reason
      : new DeployFailure(coordinator.exit || 9, "deployment interrupted while retaining the prior release");
  }
  if (coordinator.exit !== 0) {
    failDeploy(
      coordinator.exit || 5,
      `cannot prove the coordinator release before prior worker cleanup; deployment journal retained\n` +
        `${coordinator.stdout}\n${coordinator.stderr}`,
    );
  }
  const reportedCoordinatorPath = coordinator.stdout.trim();
  const coordinatorPath = reportedCoordinatorPath === "absent"
    ? null
    : reportedCoordinatorPath;
  if (coordinatorPath !== null
    && (!posix.isAbsolute(coordinatorPath) || /[\r\n\0]/.test(coordinatorPath))) {
    failDeploy(5, "coordinator WorkingDirectory is malformed; deployment journal retained");
  }
  if (!shouldRemovePriorWorkerRelease(
    prior,
    journal.targetReleasePath,
    coordinatorPath,
    home,
  )) {
    return;
  }
  const removed = await deploySsh(
    _linuxRemoveManagedWorkerReleaseCommand(prior, home),
  );
  if (signal.aborted) {
    const reason = signal.reason;
    throw reason instanceof DeployFailure
      ? reason
      : new DeployFailure(removed.exit || 9, "deployment interrupted while removing the prior release");
  }
  if (removed.exit !== 0) {
    failDeploy(
      removed.exit || 5,
      `cannot retire prior worker release ${prior}; deployment journal retained\n` +
        `${removed.stdout}\n${removed.stderr}`,
    );
  }
}

export async function _recoverLinuxDeployJournal(
  remote: LinuxDeployRecoveryRemote,
): Promise<LinuxRecoveryOutcome> {
  const journal = await remote.loadJournal();
  if (journal === null) return { kind: "none" };

  let target: LinuxDeployTargetProof = {
    healthy: false,
    proof: { exit: 1, stdout: "", stderr: "prepared stages are never committed" },
  };
  if (journal.phase !== "prepared") target = await remote.proveTarget(journal);
  const plan = linuxDeployRecoveryPlan(journal, target.healthy, remote.home);
  if (plan.kind === "clean-prepared") {
    await remote.removeTarget(journal);
    await remote.clearJournal();
    return { kind: "prepared-cleaned" };
  }
  if (plan.kind === "commit-target") {
    await remote.cleanupPrior(journal);
    await remote.clearJournal();
    return { kind: "target-committed", verification: target.proof };
  }
  await remote.restorePrior(journal);
  await remote.provePrior(journal);
  await remote.removeTarget(journal);
  await remote.clearJournal();
  return { kind: "prior-restored" };
}

async function recoverLinuxDeployJournal(
  deploySsh: DeploySsh,
  journalPath: string,
  unitPath: string,
  home: string,
  signal: AbortSignal,
): Promise<LinuxRecoveryOutcome> {
  return await _recoverLinuxDeployJournal({
    home,
    loadJournal: () => loadLinuxDeployJournal(deploySsh, journalPath, home),
    proveTarget: (journal) => proveLinuxTargetRelease(deploySsh, journal, home),
    restorePrior: async (journal) => {
      const restored = await deploySsh(
        _linuxRestorePriorServiceCommand(journal, journalPath, unitPath, home),
      );
      if (restored.exit !== 0) {
        failDeploy(
          restored.exit || 5,
          `rollback could not restore the prior Linux unit; deployment journal retained\n` +
            `${restored.stdout}\n${restored.stderr}`,
        );
      }
    },
    provePrior: async (journal) => {
      await proveLinuxPriorService(deploySsh, journal, journalPath, unitPath, home);
    },
    cleanupPrior: (journal) => removePriorLinuxWorkerRelease(
      deploySsh,
      journal,
      home,
      signal,
    ),
    removeTarget: (journal) => removeManagedLinuxWorkerRelease(
      deploySsh,
      journal.targetReleasePath,
      home,
    ),
    clearJournal: () => clearLinuxDeployJournal(deploySsh, journalPath),
  });
}

export async function deployLinux(
  host: string,
  opts: { gitSha: string; passthroughEnv: string; machineTransactionPath: string },
): Promise<void> {
  const { gitSha, passthroughEnv, machineTransactionPath } = opts;

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
    const foreignJournalGuard = await deploySsh(
      `set -e; base=${posixShellQuote(posix.dirname(journalPath))}; ` +
        `for relative in ${posixShellQuote(POSIX_WORKER_DEPLOY_JOURNAL_PATHS.local)} ` +
        `${posixShellQuote(POSIX_WORKER_DEPLOY_JOURNAL_PATHS.darwin)} ` +
        `${posixShellQuote(POSIX_WORKER_DEPLOY_JOURNAL_PATHS.coordinator)}; do ` +
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
    );
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
    }));
    if (prepared.exit !== 0) {
      try {
        const recovered = await recoverLinuxDeployJournal(
          deploySsh,
          journalPath,
          unitPath,
          home,
          deployLease.signal,
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
      if (recovered.kind === "target-committed" && recovered.verification) {
        console.warn(`   ${summary}; committed the independently verified target`);
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

    const finalTarget = await proveLinuxTargetRelease(deploySsh, {
      ...journal,
      phase: "activated",
    }, home);
    if (!finalTarget.healthy) {
      const committed = await settleActivationFailure(
        "activated worker lost exact release health before commit",
        finalTarget.proof,
      );
      finishWorkerDeploy(
        committed,
        `>> done — ${host} v2 worker deployed (linux)`,
        "linux",
      );
      return;
    }

    await removePriorLinuxWorkerRelease(
      deploySsh,
      journal,
      home,
      deployLease.signal,
    );
    const cleared = await deploySsh(_linuxClearDeployJournalCommand(journalPath));
    if (cleared.exit !== 0) {
      const recovered = await recoverLinuxDeployJournal(
        deploySsh,
        journalPath,
        unitPath,
        home,
        deployLease.signal,
      );
      if (recovered.kind === "prior-restored") {
        failDeploy(
          cleared.exit || 5,
          `commit journal cleanup failed and the prior worker service was restored\n` +
            `${cleared.stdout}\n${cleared.stderr}`,
        );
      }
      if (recovered.kind === "target-committed" && recovered.verification) {
        finishWorkerDeploy(
          recovered.verification,
          `>> done — ${host} v2 worker deployed (linux)`,
          "linux",
        );
        return;
      }
      if (recovered.kind !== "none") {
        failDeploy(cleared.exit || 5, "Linux deployment journal cleanup did not commit");
      }
    }
    finishWorkerDeploy(
      finalTarget.proof,
      `>> done — ${host} v2 worker deployed (linux)`,
      "linux",
    );
  } finally {
    await releaseRemoteDeployLock(host, machineTransactionPath, releaseId);
  }
}
