// `roost deploy <host>` routes one worker update to Windows broker, localhost,
// Linux, or macOS drivers. Atomic `roost push` supplies an internal rollout
// directive so POSIX workers hold, finalize, or roll back one exact release.

import { resolve } from "node:path";
import {
  failDeploy,
  remoteMachineTransactionPath,
  resolveLocalGitShaOrDie,
  resolvePublishedGitShaOrDie,
  resolvePinnedSourceShaOrDie,
  run,
  SSH_OPTS,
  sshExec,
} from "./deploy-exec.ts";
import { _isSelfHost } from "./deploy-self-host.ts";
import {
  _backfillEnvFromPlist,
  _resolveDeployEnvValue,
  resolveRemoteDeployIdentityEnv,
  _readWorkerServiceDefinition,
} from "./deploy-plist-env.ts";
import { _deployLocal } from "./deploy-local.ts";
import { deployLinux } from "./deploy-linux.ts";
import { deployMacosWorker } from "./deploy-macos.ts";
import {
  KEEPER_FORCE_LIVE_RETIRE_ENV,
  workerInstallEnvironment,
} from "./deploy-worker-environment.ts";
import { tryCoordinatorWindowsDeploy } from "./deploy-windows-channel.ts";
import { assertWorkerRolloutDirective } from "./worker-deploy-rollout.ts";
import type { WorkerRolloutDirective } from "./worker-deploy-rollout.ts";
import {
  createJournaledKeeperUpdateCallbacks,
  directKeeperUpdateAdmission,
  localUpdateWorkerForAdmission,
} from "./direct-keeper-update.ts";
import type {
  DirectKeeperAdmission,
  DirectKeeperAdmissionOutcome,
} from "./keeper-admission-staging.ts";
import {
  loadSourceKeeperContract,
  targetKeeperContractForWorker,
} from "./push-keeper-admission.ts";
import { resolveRemoteWorkerRuntime } from "./worker-service-runtime.ts";

import { validatePinnedDeployJobOrDie } from "./deploy-job-provenance.ts";
export { sshExec, _isSelfHost };

const REPO_ROOT = resolve(import.meta.dir, "..", "..", "..");

export interface DeployInvocationOptions {
  coordinatorUrl?: string;
  rollout?: WorkerRolloutDirective;
}

export async function deploy(
  args: string[],
  options: DeployInvocationOptions = {},
): Promise<void> {
  const host = args[0];
  if (!host) failDeploy(1, "usage: roost deploy <tailnet-host>");
  const rollout = options.rollout ? assertWorkerRolloutDirective(options.rollout) : null;
  const expectedShaArg = args.find((arg) => arg.startsWith("--expected-sha="))
    ?.slice("--expected-sha=".length);
  const expectedGitSha = rollout?.targetSha ?? expectedShaArg;
  const expectedManifestSha256 = args.find((arg) => arg.startsWith("--expected-manifest-sha256="))
    ?.slice("--expected-manifest-sha256=".length);
  const workerLabel = args.find((arg) => arg.startsWith("--label="))
    ?.slice("--label=".length);
  const reachableAddr = args.find((arg) => arg.startsWith("--reachable-addr="))
    ?.slice("--reachable-addr=".length);
  if (workerLabel !== undefined && (workerLabel === "" || /[\r\n\0]/.test(workerLabel))) {
    failDeploy(1, "--label must be a non-empty single-line worker label");
  }
  if (reachableAddr !== undefined && !/^[A-Za-z0-9._:-]+$/.test(reachableAddr)) {
    failDeploy(1, "--reachable-addr must be a hostname, FQDN, or host:port");
  }
  const sourceRootValue = args.find((arg) => arg.startsWith("--source-root="))
    ?.slice("--source-root=".length) ?? REPO_ROOT;
  if (!sourceRootValue || /[\r\n\0]/.test(sourceRootValue)) {
    failDeploy(1, "--source-root must be a local source checkout path");
  }
  const pinnedSource = args.includes("--pinned-source");
  const deployJobId = args.find((arg) => arg.startsWith("--deploy-job-id="))
    ?.slice("--deploy-job-id=".length);
  if (pinnedSource !== (deployJobId !== undefined)
    || (deployJobId !== undefined
      && !/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/.test(deployJobId))) {
    failDeploy(1, "--pinned-source requires one valid --deploy-job-id");
  }
  if (pinnedSource && expectedGitSha === undefined) {
    failDeploy(1, "--pinned-source requires --expected-sha");
  }
  const sourceCheckout = resolve(sourceRootValue);
  if (expectedGitSha !== undefined && !/^[a-f0-9]{40,64}$/i.test(expectedGitSha)) {
    failDeploy(1, "--expected-sha must be a 40-64 hex build identity");
  }
  if (expectedShaArg && rollout && expectedShaArg.toLowerCase() !== rollout.targetSha) {
    failDeploy(1, "--expected-sha does not match the worker rollout target");
  }
  if (pinnedSource && deployJobId && expectedGitSha) {
    validatePinnedDeployJobOrDie({
      jobId: deployJobId,
      workerFp: process.env.ROOST_DEPLOY_WORKER_FP,
      host,
      sourceRoot: sourceCheckout,
      targetSha: expectedGitSha,
    });
  }
  if (expectedManifestSha256 !== undefined && !/^[a-f0-9]{64}$/i.test(expectedManifestSha256)) {
    failDeploy(1, "--expected-manifest-sha256 must be a 64-hex digest");
  }

  if (rollout && /^[a-f0-9]{64}$/i.test(host)) {
    failDeploy(2, "atomic fleet rollout is unavailable for paused Windows workers");
  }
  if (!rollout && await tryCoordinatorWindowsDeploy(host, expectedGitSha, expectedManifestSha256)) return;
  if (/^[a-f0-9]{64}$/i.test(host)) {
    failDeploy(2, "registered Windows worker requires a reachable coordinator update channel");
  }
  if (process.platform === "win32") {
    failDeploy(2, "the target is not a registered Windows worker; POSIX source deploy is unavailable on Windows");
  }

  const selfHost = await _isSelfHost(host);
  const allowUnpublishedLocal = args.includes("--allow-unpublished-local");
  if (allowUnpublishedLocal && (!selfHost || rollout)) {
    failDeploy(1, "--allow-unpublished-local is restricted to the localhost quickstart path");
  }
  const forceLiveKeeperRetire = args.includes("--force-live");
  if (forceLiveKeeperRetire && rollout) {
    failDeploy(1, "--force-live is refused inside an atomic fleet rollout");
  }
  if (forceLiveKeeperRetire) {
    console.error(`--force-live authorizes ${host} to DESTROY every PTY held by a keeper`);
    console.error("  the deployed worker can neither adopt nor prove empty.");
    console.error("  Every shell, dev server, and test in those PTYs exits.");
    console.error("  It applies to this deploy only; the next deploy clears it.");
  }
  const sourceGitSha = pinnedSource
    ? resolvePinnedSourceShaOrDie(sourceCheckout, expectedGitSha!)
    : rollout
      ? rollout.targetSha
      : allowUnpublishedLocal
        ? resolveLocalGitShaOrDie(sourceCheckout)
        : resolvePublishedGitShaOrDie(sourceCheckout, expectedGitSha);
  const sourceKeeperContract = rollout
    ? null
    : await loadSourceKeeperContract(sourceCheckout, process.execPath);
  const bootstrapAllowed = allowUnpublishedLocal
    || process.env.ROOST_BOOTSTRAP_TOKEN !== undefined;
  const keeperCallbacks = createJournaledKeeperUpdateCallbacks();
  let keeperAdmission: DirectKeeperAdmission | null = rollout
    ? {
        workerFingerprint: rollout.workerFingerprint,
        keeperUpdate: rollout.keeperUpdate,
      }
    : null;
  if (selfHost) {
    await _deployLocal(host, {
      sourceRoot: sourceCheckout,
      gitSha: sourceGitSha,
      forceLiveKeeperRetire,
      coordinatorUrl: options.coordinatorUrl,
      workerLabel,
      reachableAddr,
      rollout: rollout ?? undefined,
      keeperUpdate: keeperAdmission?.keeperUpdate ?? null,
      workerFingerprint: keeperAdmission?.workerFingerprint ?? null,
      resolveKeeperAdmission: rollout
        ? undefined
        : async (runtime): Promise<DirectKeeperAdmissionOutcome> => {
            const localWorker = await localUpdateWorkerForAdmission(bootstrapAllowed);
            if (!localWorker) return { outcome: "unregistered" };
            return directKeeperUpdateAdmission(
              localWorker.fingerprint,
              targetKeeperContractForWorker(sourceKeeperContract!, sourceGitSha, {
                bun_abi: runtime.bunAbi,
                platform: runtime.platform,
                arch: runtime.arch,
              }),
              false,
            );
          },
      keeperCallbacks,
    });
    return;
  }

  console.log(`>> reachability check ssh ${host}`);
  const ssh = await run(["ssh", ...SSH_OPTS, "-o", "BatchMode=yes", "--", host, "true"]);
  if (ssh.exit !== 0) failDeploy(2, "ssh failed; ensure key-based auth to that host");
  const unameOut = await sshExec(host, "uname -s");
  const remotePlatform = unameOut.stdout.trim() === "Linux"
    ? "linux"
    : unameOut.stdout.trim() === "Darwin"
      ? "darwin"
      : null;
  if (unameOut.exit !== 0 || remotePlatform === null) {
    failDeploy(
      2,
      `unsupported deploy target platform from ${host}: ${
        unameOut.stdout.trim() || unameOut.stderr.trim() || "unknown"
      }`,
    );
  }
  const workerRuntime = await resolveRemoteWorkerRuntime(
    await _readWorkerServiceDefinition(host),
    remotePlatform,
    command => sshExec(host, command),
  );
  console.log(`   bun: ${workerRuntime.executable} @ ${workerRuntime.bunAbi}`);
  const resolveRemoteKeeperAdmission = rollout
    ? undefined
    : async (): Promise<DirectKeeperAdmissionOutcome> =>
      directKeeperUpdateAdmission(
        host,
        targetKeeperContractForWorker(sourceKeeperContract!, sourceGitSha, {
          bun_abi: workerRuntime.bunAbi,
          platform: workerRuntime.platform,
          arch: workerRuntime.arch,
        }),
        bootstrapAllowed,
      );
  if (remotePlatform === "linux") {
    const { env: hostEnv, filled } = await _backfillEnvFromPlist(host);
    if (filled.length > 0) console.log(`>> reused from the installed unit on ${host}: ${filled.join(", ")}`);
    const coordinatorUrl = _resolveDeployEnvValue(
      "ROOST_COORDINATOR_URL",
      hostEnv,
      options.coordinatorUrl,
      "remote",
    );
    if ((!rollout || rollout.action === "hold") && !coordinatorUrl) {
      failDeploy(6, "ROOST_COORDINATOR_URL env var required (no prior install on target to reuse)");
    }
    const passthroughEnv = rollout && rollout.action !== "hold"
      ? ""
      : workerInstallEnvironment(hostEnv, {
          ROOST_COORDINATOR_URL: coordinatorUrl,
          ...resolveRemoteDeployIdentityEnv(host, hostEnv, { workerLabel, reachableAddr }),
          ROOST_BOOTSTRAP_TOKEN: process.env.ROOST_BOOTSTRAP_TOKEN,
          [KEEPER_FORCE_LIVE_RETIRE_ENV]: forceLiveKeeperRetire ? "1" : undefined,
        }, sourceGitSha);
    await deployLinux(host, {
      gitSha: sourceGitSha,
      passthroughEnv,
      machineTransactionPath: remoteMachineTransactionPath("linux", hostEnv),
      bunExecutable: workerRuntime.executable,
      rollout: rollout ?? undefined,
      keeperUpdate: keeperAdmission?.keeperUpdate ?? null,
      workerFingerprint: keeperAdmission?.workerFingerprint ?? null,
      resolveKeeperAdmission: resolveRemoteKeeperAdmission,
      applyKeeperUpdate: async (
        workerFingerprint,
        update,
        direction,
      ) => {
        await keeperCallbacks.apply(workerFingerprint, update, direction);
      },
      proveKeeperUpdate: async (
        workerFingerprint,
        update,
        direction,
        expectedWorkerSha,
        heartbeatNotBeforeMs,
      ) => {
        await keeperCallbacks.prove(
          workerFingerprint,
          update,
          direction,
          expectedWorkerSha,
          heartbeatNotBeforeMs,
        );
      },
    });
    return;
  }
  if (remotePlatform !== "darwin") {
    failDeploy(2, `unsupported deploy target platform from ${host}: ${remotePlatform}`);
  }
  await deployMacosWorker(host, {
    sourceCheckout,
    gitSha: sourceGitSha,
    bunExecutable: workerRuntime.executable,
    forceLiveKeeperRetire,
    coordinatorUrl: options.coordinatorUrl,
    workerLabel,
    reachableAddr,
    rollout: rollout ?? undefined,
    keeperUpdate: keeperAdmission?.keeperUpdate ?? null,
    workerFingerprint: keeperAdmission?.workerFingerprint ?? null,
    resolveKeeperAdmission: resolveRemoteKeeperAdmission,
    applyKeeperUpdate: async (
      workerFingerprint,
      update,
      direction,
    ) => {
      await keeperCallbacks.apply(workerFingerprint, update, direction);
    },
    proveKeeperUpdate: async (
      workerFingerprint,
      update,
      direction,
      expectedWorkerSha,
      heartbeatNotBeforeMs,
    ) => {
      await keeperCallbacks.prove(
        workerFingerprint,
        update,
        direction,
        expectedWorkerSha,
        heartbeatNotBeforeMs,
      );
    },
  });
}
