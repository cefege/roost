// Filesystem and transaction location rules for local coordinator rollouts.
// The coordinator push owner uses these helpers before staging any release;
// they confine service paths, reject foreign journals, and preserve web assets.

import {
  cpSync,
  existsSync,
  lstatSync,
  mkdirSync,
  readFileSync,
  realpathSync,
} from "node:fs";
import { dirname, isAbsolute, join, resolve } from "node:path";
import { coordServicePath, roostServiceDir } from "@roost/shared/paths";
import {
  acquireMachineTransaction,
  type AcquireMachineTransactionOptions,
  type MachineTransactionLock,
} from "./machine-transaction.ts";
import {
  DeployFailure,
  failDeploy,
  POSIX_WORKER_DEPLOY_JOURNAL_PATHS,
} from "./deploy-exec.ts";
import {
  coordinatorDeployJournalPath,
  type CoordinatorDeployJournalContext,
} from "./coordinator-deploy-journal.ts";
import { coordinatorRepoFromService } from "./coordinator-service-definition.ts";

const REPO_ROOT = resolve(import.meta.dir, "..", "..", "..");

export interface CoordinatorDeployLocation {
  journalPath: string;
  context: CoordinatorDeployJournalContext;
}

function coordinatorPlatform(): "darwin" | "linux" {
  if (process.platform !== "linux" && process.platform !== "darwin") {
    failDeploy(2, "atomic fleet push requires a source-installed POSIX coordinator");
  }
  return process.platform;
}

export async function acquireFleetPushTransaction(
  location: CoordinatorDeployLocation,
  options: Pick<AcquireMachineTransactionOptions, "env" | "platform"> = {},
): Promise<MachineTransactionLock> {
  return acquireMachineTransaction("deploy", location.journalPath, {
    ...options,
    lockPath: join(location.context.transactionRoot, "fleet-push-transaction.sqlite"),
  });
}

export function prepareCoordinatorDeployLocation(): CoordinatorDeployLocation {
  const platform = coordinatorPlatform();
  const configuredServiceRoot = resolve(roostServiceDir());
  mkdirSync(configuredServiceRoot, { recursive: true, mode: 0o700 });
  const serviceRoot = realpathSync(configuredServiceRoot);
  const releaseRoot = join(serviceRoot, "releases", "coord");
  const transactionRoot = join(serviceRoot, "transactions");
  mkdirSync(releaseRoot, { recursive: true, mode: 0o700 });
  mkdirSync(transactionRoot, { recursive: true, mode: 0o700 });
  if (realpathSync(releaseRoot) !== releaseRoot
    || realpathSync(transactionRoot) !== transactionRoot) {
    failDeploy(5, "coordinator deployment directories must not traverse symbolic links");
  }
  return {
    journalPath: coordinatorDeployJournalPath(transactionRoot),
    context: {
      servicePath: resolve(coordServicePath()),
      releaseRoot,
      transactionRoot,
      platform,
    },
  };
}

export function resolveCoordinatorRepo(): string {
  const platform = coordinatorPlatform();
  const override = process.env.ROOST_COORD_REPO_DIR?.trim();
  const servicePath = coordServicePath();
  const installed = existsSync(servicePath)
    ? coordinatorRepoFromService(readFileSync(servicePath, "utf8"), platform)
    : null;
  for (const candidate of [override, installed, REPO_ROOT]) {
    if (!candidate) continue;
    const absolute = resolve(candidate);
    if (existsSync(join(absolute, ".git"))
      && existsSync(join(absolute, "apps", "coord", "scripts", "install.sh"))) {
      return realpathSync(absolute);
    }
  }
  failDeploy(2, `cannot locate the coordinator source checkout from ${servicePath}; set ROOST_COORD_REPO_DIR`);
}

export function foreignWorkerDeployJournalForCoordinator(serviceRoot: string): string | null {
  for (const relativePath of [
    POSIX_WORKER_DEPLOY_JOURNAL_PATHS.local,
    POSIX_WORKER_DEPLOY_JOURNAL_PATHS.linux,
    POSIX_WORKER_DEPLOY_JOURNAL_PATHS.darwin,
  ]) {
    const candidate = join(serviceRoot, relativePath);
    try {
      lstatSync(candidate);
      return candidate;
    } catch (error) {
      if (!(error instanceof Error
        && "code" in error
        && (error as NodeJS.ErrnoException).code === "ENOENT")) throw error;
    }
  }
  return null;
}

function hasWebDist(path: string): boolean {
  return existsSync(join(path, "index.html"));
}

export function preserveWebDistForNoBuild(
  releaseDir: string,
  installedEnvironment: Readonly<Record<string, string>>,
  priorRepo: string,
): string {
  const destination = join(releaseDir, "apps", "web", "dist");
  if (hasWebDist(destination)) return destination;
  const configured = installedEnvironment.ROOST_WEB_DIST_PATH;
  for (const candidate of [
    configured ? (isAbsolute(configured) ? configured : resolve(priorRepo, configured)) : undefined,
    join(priorRepo, "apps", "web", "dist"),
  ]) {
    if (!candidate || resolve(candidate) === resolve(destination) || !hasWebDist(candidate)) continue;
    mkdirSync(dirname(destination), { recursive: true });
    cpSync(candidate, destination, { recursive: true, dereference: true });
    return destination;
  }
  throw new DeployFailure(
    5,
    "--no-web requested but neither the staged commit nor the installed coordinator has apps/web/dist",
  );
}
