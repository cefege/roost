// `roost deploy <host>` routes one worker update to Windows broker, localhost,
// Linux, or macOS drivers. Atomic `roost push` supplies an internal rollout
// directive so POSIX workers hold, finalize, or roll back one exact release.

import { resolve } from "node:path";
import {
  failDeploy,
  remoteMachineTransactionPath,
  resolveLocalGitShaOrDie,
  resolvePublishedGitShaOrDie,
  run,
  SSH_OPTS,
  sshExec,
} from "./deploy-exec.ts";
import { _isSelfHost } from "./deploy-self-host.ts";
import { _backfillEnvFromPlist, _resolveDeployEnvValue } from "./deploy-plist-env.ts";
import { _deployLocal } from "./deploy-local.ts";
import { deployLinux } from "./deploy-linux.ts";
import { deployMacosWorker } from "./deploy-macos.ts";
import { workerInstallEnvironment } from "./deploy-worker-environment.ts";
import { tryCoordinatorWindowsDeploy } from "./deploy-windows-channel.ts";
import { assertWorkerRolloutDirective } from "./worker-deploy-rollout.ts";
import type { WorkerRolloutDirective } from "./worker-deploy-rollout.ts";

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
  const sourceRootValue = args.find((arg) => arg.startsWith("--source-root="))
    ?.slice("--source-root=".length) ?? REPO_ROOT;
  if (!sourceRootValue || /[\r\n\0]/.test(sourceRootValue)) {
    failDeploy(1, "--source-root must be a local source checkout path");
  }
  const sourceCheckout = resolve(sourceRootValue);
  if (expectedGitSha !== undefined && !/^[a-f0-9]{40,64}$/i.test(expectedGitSha)) {
    failDeploy(1, "--expected-sha must be a 40-64 hex build identity");
  }
  if (expectedShaArg && rollout && expectedShaArg.toLowerCase() !== rollout.targetSha) {
    failDeploy(1, "--expected-sha does not match the worker rollout target");
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
  const sourceGitSha = rollout
    ? rollout.targetSha
    : allowUnpublishedLocal
      ? resolveLocalGitShaOrDie(sourceCheckout)
      : resolvePublishedGitShaOrDie(sourceCheckout, expectedGitSha);
  if (selfHost) {
    await _deployLocal(host, {
      sourceRoot: sourceCheckout,
      gitSha: sourceGitSha,
      coordinatorUrl: options.coordinatorUrl,
      rollout: rollout ?? undefined,
    });
    return;
  }

  console.log(`>> reachability check ssh ${host}`);
  const ssh = await run(["ssh", ...SSH_OPTS, "-o", "BatchMode=yes", "--", host, "true"]);
  if (ssh.exit !== 0) failDeploy(2, "ssh failed; ensure key-based / tailscale-ssh auth");
  console.log(`>> verify bun on ${host}`);
  const bunCheck = await sshExec(host, "command -v bun && bun --version");
  if (bunCheck.exit !== 0) {
    failDeploy(3, `bun not found in remote login shell. Install: curl -fsSL https://bun.sh/install | bash\n${bunCheck.stderr}`);
  }
  console.log(`   bun: ${bunCheck.stdout.trim().split("\n").slice(-2).join(" @ ")}`);

  const unameOut = await sshExec(host, "uname -s");
  if (unameOut.stdout.trim() === "Linux") {
    const { env: hostEnv, filled } = await _backfillEnvFromPlist(host);
    if (filled.length > 0) console.log(`>> reused from the installed unit on ${host}: ${filled.join(", ")}`);
    const resolved = (key: string, invocationValue?: string): string | undefined =>
      _resolveDeployEnvValue(key, hostEnv, invocationValue);
    const coordinatorUrl = resolved("ROOST_COORDINATOR_URL", options.coordinatorUrl);
    if ((!rollout || rollout.action === "hold") && !coordinatorUrl) {
      failDeploy(6, "ROOST_COORDINATOR_URL env var required (no prior install on target to reuse)");
    }
    const passthroughEnv = rollout && rollout.action !== "hold"
      ? ""
      : workerInstallEnvironment(hostEnv, {
          ROOST_COORDINATOR_URL: coordinatorUrl,
          ROOST_WORKER_LABEL: resolved("ROOST_WORKER_LABEL"),
          ROOST_REACHABLE_ADDR: resolved("ROOST_REACHABLE_ADDR"),
          ROOST_BOOTSTRAP_TOKEN: process.env.ROOST_BOOTSTRAP_TOKEN,
        }, sourceGitSha);
    await deployLinux(host, {
      gitSha: sourceGitSha,
      passthroughEnv,
      machineTransactionPath: remoteMachineTransactionPath("linux", hostEnv),
      rollout: rollout ?? undefined,
    });
    return;
  }
  if (unameOut.exit !== 0 || unameOut.stdout.trim() !== "Darwin") {
    failDeploy(2, `unsupported deploy target platform from ${host}: ${unameOut.stdout.trim() || unameOut.stderr.trim() || "unknown"}`);
  }
  await deployMacosWorker(host, {
    sourceCheckout,
    gitSha: sourceGitSha,
    coordinatorUrl: options.coordinatorUrl,
    rollout: rollout ?? undefined,
  });
}
