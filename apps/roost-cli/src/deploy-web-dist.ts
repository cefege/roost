// Builds the SPA the worker's own loopback UI door serves, inside the release a
// deploy just staged. apps/web/dist is gitignored, so no checkout or rsync ever
// carries one and a worker without it answers `/` with a 404 the browser
// downloads instead of rendering. Each backend (deploy-linux.ts,
// deploy-macos.ts, deploy-local.ts) runs this step between dependency install
// and service activation, then stamps the returned directory as
// ROOST_WEB_DIST_PATH so the installed service names the release it built.

import { existsSync } from "node:fs";
import { join, posix } from "node:path";
import { DeployFailure, failDeploy, run } from "./deploy-exec.ts";
import { posixShellQuote } from "@roost/shared/shell-quote";

export interface WorkerSpaCommandResult {
  exit: number;
  stdout: string;
  stderr: string;
}

/** Hands a failed build to the backend's own failure path — journal settlement
 *  on Linux, stage cleanup elsewhere. It is expected never to return, and its
 *  own result is discarded. */
export type SettleWorkerSpaFailure = (
  failure: string,
  result: WorkerSpaCommandResult,
) => Promise<unknown>;

/** The convention of both backends that stage a release before activating it:
 *  discard the stage, then end the deploy with the build's own output. */
export function cleanupStageOnSpaFailure(
  cleanupStage: () => Promise<void>,
): SettleWorkerSpaFailure {
  return async (failure, result) => {
    await cleanupStage();
    failDeploy(4, `${failure}\n${result.stdout}\n${result.stderr}`);
  };
}

const SPA_BUILD_FAILED = "worker SPA build failed";

export async function buildStagedWorkerSpaOverSsh(deps: {
  /** How the step names its target: `on ${host}`. */
  label: string;
  /** The staged release as shell text — a quoted absolute path, or a remote
   *  stage's unexpanded `~/…` which the remote shell must still expand. */
  releaseDirectory: string;
  /** Exact executable admitted from the installed service or first-install discovery. */
  bunExecutable: string;
  execute: (command: string) => Promise<WorkerSpaCommandResult>;
  settle: SettleWorkerSpaFailure;
}): Promise<string> {
  console.log(`>> build worker SPA ${deps.label}`);
  const built = await deps.execute(
    `set -eo pipefail; cd ${deps.releaseDirectory} && ${posixShellQuote(deps.bunExecutable)} run --cwd apps/web build 2>&1 | tail -25`,
  );
  if (built.exit !== 0) await refuseActivation(deps.settle, SPA_BUILD_FAILED, built);
  // A zero-exit build that produced no index is still a shut door. `pwd -P`
  // both proves the directory is reachable and resolves it exactly as
  // install.sh resolves its own REPO_ROOT, which is what turns a stage spelled
  // `~/…` into the absolute path a service definition can carry.
  const proof = await deps.execute(
    `set -e; cd ${deps.releaseDirectory}/apps/web/dist && test -f index.html && pwd -P`,
  );
  const distPath = proof.stdout.trim();
  if (proof.exit !== 0 || !posix.isAbsolute(distPath) || /[\r\n\0]/.test(distPath)) {
    // `built` carries the build's own tail, which is what diagnoses a build
    // that exited clean and emitted nothing; the probe's stderr is noise.
    await refuseActivation(
      deps.settle,
      `${SPA_BUILD_FAILED}: no ${deps.releaseDirectory}/apps/web/dist/index.html`,
      built,
    );
  }
  console.log(`   worker SPA built in ${distPath}`);
  return distPath;
}

export async function buildStagedWorkerSpaLocally(deps: {
  /** Staged release path, already resolved: the localhost backend proves it
   *  traverses no symlink before this step. */
  releaseDirectory: string;
  bunBin: string;
  settle: SettleWorkerSpaFailure;
}): Promise<string> {
  console.log(">> build worker SPA locally");
  const built = await run([deps.bunBin, "run", "--cwd", "apps/web", "build"], {
    cwd: deps.releaseDirectory,
    quiet: true,
  });
  if (built.exit !== 0) await refuseActivation(deps.settle, SPA_BUILD_FAILED, built);
  const distPath = join(deps.releaseDirectory, "apps", "web", "dist");
  if (!existsSync(join(distPath, "index.html"))) {
    await refuseActivation(deps.settle, `${SPA_BUILD_FAILED}: no ${join(distPath, "index.html")}`, built);
  }
  console.log(`   worker SPA built in ${distPath}`);
  return distPath;
}

/** A backend settles its own failure and is expected to end the deploy there;
 *  one that returns anyway must not carry on into activation, because that
 *  installs a worker whose only answer at `/` is a download. */
async function refuseActivation(
  settle: SettleWorkerSpaFailure,
  failure: string,
  result: WorkerSpaCommandResult,
): Promise<never> {
  await settle(failure, result);
  throw new DeployFailure(result.exit || 4, failure);
}
