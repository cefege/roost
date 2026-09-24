// Source proof for worker deploys the coordinator starts on its own (catch-up
// and the Machines "Update" button): the source checkout must be the installed
// coordinator release, running and checked out at the required SHA. Called by
// `deploy.ts` when `--coordinator-release` is passed; depends on the installed
// coordinator service definition (systemd unit or launchd plist).
import { existsSync, readFileSync, realpathSync } from "node:fs";
import { resolve } from "node:path";
import { coordServicePath } from "@roost/shared/paths";
import { failDeploy, resolveLocalGitShaOrDie } from "./deploy-exec.ts";
import {
  coordinatorInstallEnvironment,
  coordinatorRepoFromService,
} from "./coordinator-service-definition.ts";

export interface CoordinatorServiceLocation {
  readonly path: string;
  readonly platform: NodeJS.Platform;
}

/** The installed coordinator SHA was proven published by `roost push` before
 *  install, so no upstream-tip or fetch check is repeated here: the proof is
 *  that `sourceCheckout` IS that release and its clean HEAD is `expectedSha`. */
export function resolveCoordinatorReleaseGitShaOrDie(
  sourceCheckout: string,
  expectedSha: string,
  service: CoordinatorServiceLocation = { path: coordServicePath(), platform: process.platform },
): string {
  const platform = service.platform;
  if (platform !== "linux" && platform !== "darwin") {
    failDeploy(2, "coordinator release deploys require a POSIX coordinator");
  }
  if (!existsSync(service.path)) {
    failDeploy(7, `coordinator release proof: service definition ${service.path} is missing`);
  }
  const definition = readFileSync(service.path, "utf8");
  const workingDirectory = coordinatorRepoFromService(definition, platform);
  if (workingDirectory === null) {
    failDeploy(7, "coordinator release proof: service definition has no WorkingDirectory");
  }
  const sourceReal = realpathOrDie(sourceCheckout);
  const installedReal = realpathOrDie(workingDirectory);
  if (sourceReal !== installedReal) {
    failDeploy(
      7,
      `coordinator release proof: source ${sourceCheckout} is not the installed coordinator release ${workingDirectory}`,
    );
  }
  const installed = coordinatorInstallEnvironment(definition, platform);
  const installedSha = (installed.ROOST_GIT_SHA ?? installed.GIT_SHA)?.toLowerCase();
  if (installedSha !== expectedSha.toLowerCase()) {
    failDeploy(
      7,
      `coordinator release proof: installed coordinator runs ${installedSha?.slice(0, 8) ?? "no SHA"}, not ${expectedSha.slice(0, 8)}`,
    );
  }
  const sha = resolveLocalGitShaOrDie(sourceCheckout);
  if (sha.endsWith("-dirty")) {
    failDeploy(7, "coordinator release proof: the coordinator release tree is not clean");
  }
  if (sha.toLowerCase() !== expectedSha.toLowerCase()) {
    failDeploy(
      7,
      `coordinator release proof: source HEAD ${sha.slice(0, 8)} does not match required build ${expectedSha.slice(0, 8)}`,
    );
  }
  return sha.toLowerCase();
}

function realpathOrDie(path: string): string {
  try {
    return realpathSync(resolve(path));
  } catch (error) {
    return failDeploy(7, `coordinator release proof: cannot resolve ${path}: ${String(error)}`);
  }
}
