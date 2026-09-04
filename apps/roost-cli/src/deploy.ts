// `roost deploy <host>` — refresh the v2 worker on a tailnet host.
//
// macOS target: rsync the worker tree, `bun install`, (re)install the
// LaunchAgent via apps/worker/scripts/install.sh, kickstart it. Idempotent —
// first deploy installs, subsequent deploys just refresh + kickstart.
//
// Layout on remote:
//   ~/RoostWorkerV2/
//     apps/worker/...        (rsynced)
//     apps/shared/...        (rsynced)
//     package.json + bun.lock
//     node_modules/          (bun install)
//
// Linux target: forks to deploy-linux.ts, which updates the git checkout
// join.sh left at /srv/roost and drives the systemd --user unit instead.
//
// LaunchAgent label: com.roost.worker-v2 (avoids collision with legacy
// com.roost.worker until R6.5).

import { existsSync, mkdtempSync, rmSync } from "node:fs";
import { homedir, tmpdir } from "node:os";
import { join, posix, resolve } from "node:path";
import { acquireRemoteDeployLock, DeployFailure, failDeploy, finishWorkerDeploy, POSIX_WORKER_DEPLOY_JOURNAL_PATHS, releaseRemoteDeployLock, remoteMachineTransactionPath, run, runOrDie, SSH_OPTS, RSYNC_RSH, sshExec, resolveLocalGitShaOrDie, resolvePublishedGitShaOrDie } from "./deploy-exec.ts";
import { _isSelfHost } from "./deploy-self-host.ts";
import { _backfillEnvFromPlist, _resolveDeployEnvValue } from "./deploy-plist-env.ts";
import { _deployLocal } from "./deploy-local.ts";
import { manifestOnlyWorkspaces } from "./deploy-workspaces.ts";
import { deployLinux } from "./deploy-linux.ts";
import { posixShellQuote } from "@roost/shared/shell-quote";
import {
  MACOS_WORKER_LABEL,
  _macosDeployJournalPath,
  _recoverMacosDeployJournal,
  type MacosDeployRecoveryResult,
} from "./deploy-macos-journal.ts";
import { createMacosDeployJournalController } from "./deploy-macos-journal-controller.ts";
import { tryCoordinatorWindowsDeploy } from "./deploy-windows-channel.ts";

// Re-export the public surface external modules import from ./deploy.ts —
// keeper-refresh.ts pulls { _isSelfHost, sshExec }; main/push/quickstart use deploy.
export { sshExec, _isSelfHost };

const REMOTE_DIR = "~/RoostWorkerV2";
const REPO_ROOT = resolve(import.meta.dir, "..", "..", "..");

function remoteEnvAssignment(key: string, value: string): string {
  return `${key}=${posixShellQuote(value)}`;
}

function workerInstallEnvironment(
  installed: Record<string, string>,
  overrides: Record<string, string | undefined>,
  gitSha: string,
): string {
  const values: Record<string, string> = { ...installed };
  // These identify the active release itself and must point at the new stage,
  // never at the prior service's working directory or compiled binary.
  for (const key of ["GIT_SHA", "ROOST_GIT_SHA", "ROOST_WORKDIR", "ROOST_EXEC_BIN", "ROOST_BOOTSTRAP_TOKEN"]) {
    delete values[key];
  }
  for (const [key, value] of Object.entries(overrides)) {
    if (value === undefined) delete values[key];
    else values[key] = value;
  }
  values.GIT_SHA = gitSha;
  return Object.entries(values)
    .filter(([key]) => key === "GIT_SHA" || /^ROOST_[A-Z_]+$/.test(key))
    .map(([key, value]) => remoteEnvAssignment(key, value))
    .join(" ");
}

export async function deploy(args: string[]): Promise<void> {
  const host = args[0];
  if (!host) failDeploy(1, "usage: roost deploy <tailnet-host>");
  const expectedGitSha = args.find((arg) => arg.startsWith("--expected-sha="))
    ?.slice("--expected-sha=".length);
  const expectedManifestSha256 = args.find((arg) => arg.startsWith("--expected-manifest-sha256="))
    ?.slice("--expected-manifest-sha256=".length);
  const sourceRootValue = args.find((arg) => arg.startsWith("--source-root="))
    ?.slice("--source-root=".length) ?? REPO_ROOT;
  if (!sourceRootValue || /[\r\n\0]/.test(sourceRootValue)) {
    failDeploy(1, "--source-root must be a local source checkout path");
  }
  const sourceCheckout = resolve(sourceRootValue);
  if (expectedGitSha !== undefined && !/^[a-f0-9]{40,64}$/i.test(expectedGitSha)) {
    failDeploy(1, "--expected-sha must be a 40-64 hex build identity");
  }
  if (expectedManifestSha256 !== undefined && !/^[a-f0-9]{64}$/i.test(expectedManifestSha256)) {
    failDeploy(1, "--expected-manifest-sha256 must be a 64-hex digest");
  }
  if (await tryCoordinatorWindowsDeploy(
    host,
    expectedGitSha,
    expectedManifestSha256,
  )) return;
  if (/^[a-f0-9]{64}$/i.test(host)) {
    failDeploy(2, "registered Windows worker requires a reachable coordinator update channel");
  }
  if (process.platform === "win32") {
    failDeploy(2, "the target is not a registered Windows worker; POSIX source deploy is unavailable on Windows");
  }

  const selfHost = await _isSelfHost(host);
  const allowUnpublishedLocal = args.includes("--allow-unpublished-local");
  if (allowUnpublishedLocal && !selfHost) {
    failDeploy(1, "--allow-unpublished-local is restricted to the localhost quickstart path");
  }
  const sourceGitSha = allowUnpublishedLocal
    ? resolveLocalGitShaOrDie(sourceCheckout)
    : resolvePublishedGitShaOrDie(sourceCheckout, expectedGitSha);
  if (selfHost) {
    await _deployLocal(host, { sourceRoot: sourceCheckout, gitSha: sourceGitSha });
    return;
  }

  console.log(`>> reachability check ssh ${host}`);
  const ssh = await run(
    ["ssh", ...SSH_OPTS, "-o", "BatchMode=yes", "--", host, "true"],
  );
  if (ssh.exit !== 0) {
    failDeploy(2, "ssh failed; ensure key-based / tailscale-ssh auth");
  }

  console.log(`>> verify bun on ${host}`);
  const bunCheck = await sshExec(host, "command -v bun && bun --version");
  if (bunCheck.exit !== 0) {
    failDeploy(
      3,
      `bun not found in remote login shell. Install: curl -fsSL https://bun.sh/install | bash\n${bunCheck.stderr}`,
    );
  }
  console.log(`   bun: ${bunCheck.stdout.trim().split("\n").slice(-2).join(" @ ")}`);

  // Linux targets hold a full git checkout (join.sh clones the repo), so
  // they update in place — no rsync of a slim tree, no tailscale cert.
  const unameOut = await sshExec(host, "uname -s");
  if (unameOut.stdout.trim() === "Linux") {
    const gitSha = sourceGitSha;
    const { env: hostEnv, filled } = await _backfillEnvFromPlist(host);
    if (filled.length > 0) {
      console.log(`>> reused from the installed unit on ${host}: ${filled.join(", ")}`);
    }
    // Installed target identity wins; ambient values only seed fresh installs.
    const resolved = (key: string): string | undefined => _resolveDeployEnvValue(key, hostEnv);
    if (!resolved("ROOST_COORDINATOR_URL")) {
      failDeploy(6, "ROOST_COORDINATOR_URL env var required (no prior install on target to reuse)");
    }
    const passthroughEnv = workerInstallEnvironment(hostEnv, {
      ROOST_COORDINATOR_URL: resolved("ROOST_COORDINATOR_URL"),
      ROOST_WORKER_LABEL: resolved("ROOST_WORKER_LABEL"),
      ROOST_REACHABLE_ADDR: resolved("ROOST_REACHABLE_ADDR"),
      ROOST_BOOTSTRAP_TOKEN: process.env.ROOST_BOOTSTRAP_TOKEN,
    }, gitSha);
    await deployLinux(host, {
      gitSha,
      passthroughEnv,
      machineTransactionPath: remoteMachineTransactionPath("linux", hostEnv),
    });
    return;
  }
  if (unameOut.exit !== 0 || unameOut.stdout.trim() !== "Darwin") {
    failDeploy(
      2,
      `unsupported deploy target platform from ${host}: ${unameOut.stdout.trim() || unameOut.stderr.trim() || "unknown"}`,
    );
  }

  // Resolve every local and installed prerequisite before the first remote
  // write. A missing URL or unprovable source identity leaves the live tree.
  const localGitSha = sourceGitSha;
  const releaseId = `${localGitSha}-${crypto.randomUUID()}`;
  const remoteDir = `${REMOTE_DIR}-releases/${releaseId}`;
  const { env: hostEnv, filled } = await _backfillEnvFromPlist(host);
  if (filled.length > 0) {
    console.log(`>> reused from existing plist on ${host}: ${filled.join(", ")}`);
  }
  const resolved = (key: string): string | undefined => _resolveDeployEnvValue(key, hostEnv);
  const resolvedCoordinatorUrl = resolved("ROOST_COORDINATOR_URL");
  if (!resolvedCoordinatorUrl) {
    failDeploy(
      6,
      `ROOST_COORDINATOR_URL env var required (no prior plist on target to reuse). ` +
        `Set it before running: roost deploy ${host}`,
    );
  }
  const resolvedReachableAddr = resolved("ROOST_REACHABLE_ADDR");
  if (localGitSha.endsWith("-dirty")) {
    failDeploy(7, "a macOS deploy requires a clean committed source snapshot");
  }
  const snapshotParent = mkdtempSync(join(tmpdir(), "roost-deploy-source-"));
  const sourceRoot = join(snapshotParent, "source");
  try {
    await runOrDie(
      ["git", "-C", sourceCheckout, "worktree", "add", "--quiet", "--force", "--detach", sourceRoot, localGitSha],
      "local source snapshot",
    );
  } catch (error) {
    rmSync(snapshotParent, { recursive: true, force: true });
    throw error;
  }
  const cleanupSource = async (): Promise<void> => {
    await run(["git", "-C", sourceCheckout, "worktree", "remove", "--force", sourceRoot], { quiet: true });
    rmSync(snapshotParent, { recursive: true, force: true });
  };
  try {

  const deployLock = remoteMachineTransactionPath("darwin", hostEnv);
  const deployLease = await acquireRemoteDeployLock(host, deployLock, releaseId);
  const deploySsh = (command: string) => sshExec(host, command, deployLease.signal);
  try {
  const deployLockSpec = posix.normalize(deployLock);
  const deployLockBase = posix.dirname(deployLockSpec);
  const foreignJournalGuard = await deploySsh(
    `set -e; base_spec=${posixShellQuote(deployLockBase)}; ` +
      `case "$base_spec" in /*) base="$base_spec";; *) base="$HOME/$base_spec";; esac; ` +
      `for relative in ${posixShellQuote(POSIX_WORKER_DEPLOY_JOURNAL_PATHS.local)} ` +
      `${posixShellQuote(POSIX_WORKER_DEPLOY_JOURNAL_PATHS.linux)} ` +
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
  const deployJournal = _macosDeployJournalPath(deployLock);
  const journalController = createMacosDeployJournalController(
    deploySsh,
    deployJournal,
    deployLease.signal,
  );
  const interrupted = await _recoverMacosDeployJournal(journalController.recovery);
  if (interrupted.outcome === "prepared-cleaned") {
    console.log(">> cleaned an interrupted prepared macOS release");
  } else if (interrupted.outcome === "rolled-back") {
    console.log(">> restored the prior macOS worker from an interrupted activation");
  } else if (interrupted.outcome === "committed") {
    console.log(">> committed a previously activated healthy macOS worker");
  }
  console.log(`>> ensure staged release ${remoteDir}/ on ${host}`);
  // Manifest-only workspaces are derived, never enumerated (deploy-workspaces.ts).
  const manifestOnly = manifestOnlyWorkspaces(sourceRoot);
  const stage = await deploySsh(`mkdir -p ${manifestOnly.map((w) => `${remoteDir}/${w}`).join(" ")}`);
  if (stage.exit !== 0) {
    failDeploy(stage.exit || 4, `cannot create macOS release stage\n${stage.stdout}\n${stage.stderr}`);
  }
  const cleanupStage = async (): Promise<void> => {
    await deploySsh(`rm -rf ${remoteDir}`);
  };

  // Every worker ships the coordinator too: CoordTarget resolves the installer
  // and the SPA build against process.cwd(), so a Mac that only has
  // apps/worker can never accept a coordinator move. The Linux path already
  // deploys a full checkout — this keeps the two consistent.
  console.log(`>> rsync canonical workspace + apps/{worker,shared,coord,web}/ + vendor/ → ${host}:${remoteDir}/`);
  const rsyncs = await Promise.allSettled([
    runOrDie([
      "rsync", "-az", "-e", RSYNC_RSH, "--delete",
      "--exclude", "node_modules", "--exclude", "tests", "--exclude", "test-results",
      `${sourceRoot}/apps/worker/`, `${host}:${remoteDir}/apps/worker/`,
    ], "rsync apps/worker", deployLease.signal),
    runOrDie([
      "rsync", "-az", "-e", RSYNC_RSH, "--delete",
      "--exclude", "node_modules",
      `${sourceRoot}/apps/shared/`, `${host}:${remoteDir}/apps/shared/`,
    ], "rsync apps/shared", deployLease.signal),
    runOrDie([
      "rsync", "-az", "-e", RSYNC_RSH, "--delete",
      "--exclude", "node_modules", "--exclude", "tests", "--exclude", "test-results",
      `${sourceRoot}/apps/coord/`, `${host}:${remoteDir}/apps/coord/`,
    ], "rsync apps/coord", deployLease.signal),
    runOrDie([
      // dist is excluded on purpose: CoordTarget builds it on the target at
      // PREPARE, and a stale copy would be served in preference to that.
      "rsync", "-az", "-e", RSYNC_RSH, "--delete",
      "--exclude", "node_modules", "--exclude", "dist", "--exclude", "tests",
      `${sourceRoot}/apps/web/`, `${host}:${remoteDir}/apps/web/`,
    ], "rsync apps/web", deployLease.signal),
    ...(existsSync(join(sourceRoot, "vendor"))
      ? [runOrDie([
          "rsync", "-az", "-e", RSYNC_RSH, "--delete",
          "--exclude", "node_modules", "--exclude", "dist",
          `${sourceRoot}/vendor/`, `${host}:${remoteDir}/vendor/`,
        ], "rsync vendor", deployLease.signal)]
      : []),
    runOrDie([
      // Canonical workspace manifests + lock preserve artifact identity for a
      // given Git SHA. Empty source dirs are enough for omitted workspaces.
      "rsync", "-az", "-e", RSYNC_RSH,
      `${sourceRoot}/package.json`, `${sourceRoot}/bun.lock`,
      `${sourceRoot}/tsconfig.base.json`, `${sourceRoot}/bunfig.toml`,
      `${host}:${remoteDir}/`,
    ], "rsync root workspace", deployLease.signal),
    ...manifestOnly.map((relative) => runOrDie([
      "rsync", "-az", "-e", RSYNC_RSH,
      `${sourceRoot}/${relative}/package.json`, `${host}:${remoteDir}/${relative}/`,
    ], `rsync ${relative} manifest`, deployLease.signal)),
  ]);
  const rejectedRsync = rsyncs.find(
    (result): result is PromiseRejectedResult => result.status === "rejected",
  );
  if (rejectedRsync) {
    await cleanupStage();
    throw rejectedRsync.reason;
  }


  // NOT --production: apps/web's build needs vite + vite-plugin-solid, which
  // are devDependencies. Without them a coordinator move fails at PREPARE.
  console.log(`>> bun install on ${host}`);
  const installRes = await deploySsh(
    // `set -o pipefail` so the tail filter doesn't mask a non-zero exit.
    `set -eo pipefail; cd ${remoteDir} && bun install --frozen-lockfile 2>&1 | tail -25`,
  );
  if (installRes.exit !== 0) {
    await cleanupStage();
    failDeploy(
      4,
      `bun install failed\n${installRes.stdout}\n${installRes.stderr}`,
    );
  }
  console.log("   bun install ok");


  const passthroughEnv = workerInstallEnvironment(hostEnv, {
    ROOST_COORDINATOR_URL: resolvedCoordinatorUrl,
    ROOST_WORKER_LABEL: resolved("ROOST_WORKER_LABEL"),
    ROOST_BOOTSTRAP_TOKEN: process.env.ROOST_BOOTSTRAP_TOKEN,
    ROOST_REACHABLE_ADDR: resolvedReachableAddr,
  }, localGitSha);
  await journalController.prepare(localGitSha, remoteDir);
  // Atomic checkpoint is the final operation before install.sh can rewrite or
  // boot out the live LaunchAgent.
  await journalController.checkpointActivating(localGitSha, remoteDir);

  const throwIfActivationTransportLost = (
    result: { exit: number; stdout: string; stderr: string },
    operation: string,
  ): void => {
    if (!deployLease.signal.aborted && result.exit !== 255 && result.exit < 128) return;
    const reason = deployLease.signal.reason;
    if (reason instanceof DeployFailure) throw reason;
    throw new DeployFailure(
      result.exit || 9,
      `${operation} lost its remote shell; durable macOS deploy journal retained\n` +
        `${result.stdout}\n${result.stderr}`,
    );
  };
  const recoverFailedActivation = async (
    exitCode: number,
    failure: string,
  ): Promise<void> => {
    let recovery: MacosDeployRecoveryResult;
    try {
      recovery = await _recoverMacosDeployJournal(journalController.recovery);
    } catch (error) {
      if (error instanceof DeployFailure
        && (deployLease.signal.aborted || error.exitCode === 255 || error.exitCode >= 128)) {
        throw error;
      }
      failDeploy(
        exitCode,
        `${failure}\nrollback failed; durable macOS deploy journal retained\n` +
          (error instanceof Error ? error.message : String(error)),
      );
    }
    if (recovery.outcome === "committed") {
      finishWorkerDeploy(
        recovery.targetProof.result,
        `>> done — ${host} v2 worker deployed`,
        "darwin",
      );
      return;
    }
    if (recovery.outcome === "rolled-back") {
      failDeploy(exitCode, `${failure}\nprior worker service restored`);
    }
    failDeploy(exitCode, `${failure}\nmacOS deploy journal disappeared before recovery`);
  };

  const installSh = await deploySsh(
    `${passthroughEnv} bash ${remoteDir}/apps/worker/scripts/install.sh install 2>&1`,
  );
  if (installSh.exit !== 0) {
    throwIfActivationTransportLost(installSh, "install macOS worker");
    await recoverFailedActivation(
      5,
      `install.sh failed\n${installSh.stdout}\n${installSh.stderr}`,
    );
    return;
  }
  console.log(installSh.stdout.trim().split("\n").map((l) => `   ${l}`).join("\n"));

  console.log(`>> kickstart ${MACOS_WORKER_LABEL} on ${host}`);
  // install.sh already kickstarts, but bounce again after rsync to be sure.
  const kick = await deploySsh(
    `launchctl kickstart -k gui/$(id -u)/${MACOS_WORKER_LABEL} 2>&1`,
  );
  if (kick.exit !== 0) {
    throwIfActivationTransportLost(kick, "kickstart macOS worker");
    await recoverFailedActivation(
      4,
      `kickstart failed (exit ${kick.exit})\n${kick.stdout}\n${kick.stderr}`,
    );
    return;
  }

  console.log(`>> verifying service is up on ${host}`);
  const { promise: settled, resolve: markSettled } = Promise.withResolvers<void>();
  setTimeout(markSettled, 1500);
  await settled;
  const activation = await _recoverMacosDeployJournal(journalController.recovery);
  if (activation.outcome === "committed") {
    finishWorkerDeploy(
      activation.targetProof.result,
      `>> done — ${host} v2 worker deployed`,
      "darwin",
    );
  } else if (activation.outcome === "rolled-back") {
    const proof = activation.targetProof.result;
    failDeploy(
      proof.exit || 8,
      `worker service verification failed\n${proof.stdout}\n${proof.stderr}\n` +
        "prior worker service restored",
    );
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
