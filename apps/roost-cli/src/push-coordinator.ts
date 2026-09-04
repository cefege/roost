// Local POSIX coordinator staging and activation for an atomic fleet push.
// It snapshots the live database, installs an immutable target, and returns
// only after the coordinator journal is durably held at fleet-converging.

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
import { durableRemove } from "@roost/shared/durability";
import { acquireMachineTransaction } from "./machine-transaction.ts";
import type {
  AcquireMachineTransactionOptions,
  MachineTransactionLock,
} from "./machine-transaction.ts";
import {
  DeployFailure,
  failDeploy,
  POSIX_WORKER_DEPLOY_JOURNAL_PATHS,
  runOrDie,
} from "./deploy-exec.ts";
import {
  COORDINATOR_DEPLOY_JOURNAL_SCHEMA_VERSION,
  canonicalCoordinatorTargetWorkers,
  coordinatorDatabaseSnapshotPath,
  coordinatorDeployJournalPath,
  loadCoordinatorDeployJournal,
  parseCoordinatorDeployJournal,
  writeCoordinatorDeployJournal,
  writeCoordinatorDeployPhase,
  type CoordinatorDeployJournalContext,
  type CoordinatorDeployJournalV2,
} from "./coordinator-deploy-journal.ts";
import {
  coordinatorInstallEnvironment,
  coordinatorRepoFromService,
} from "./coordinator-service-definition.ts";
import {
  coordinatorReportIsHealthy,
  coordinatorStartupPolicyIsEnabled,
  flushCoordinatorReleaseTree,
  markCoordinatorFleetConverging,
  removeStagedCoordinatorRelease,
  rollbackCoordinatorDeploy,
} from "./coordinator-deploy-recovery.ts";
import { createCoordinatorRollbackSnapshot } from "./coordinator-deploy-snapshot.ts";
import { statusReport } from "./status.ts";
import { assertWorkerRolloutDirective } from "./worker-deploy-rollout.ts";

const REPO_ROOT = resolve(import.meta.dir, "..", "..", "..");
export interface CoordinatorDeployLocation {
  journalPath: string;
  context: CoordinatorDeployJournalContext;
}
export interface HeldCoordinatorDeploy extends CoordinatorDeployLocation {
  journal: CoordinatorDeployJournalV2;
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

function resolveCoordinatorRepo(): string {
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

async function removeUnjournaledStage(
  location: CoordinatorDeployLocation,
  stagingRepoPath: string,
  stagedReleasePath: string,
  targetSha: string,
  snapshotPath?: string,
): Promise<void> {
  await removeStagedCoordinatorRelease(
    location.context.releaseRoot,
    stagingRepoPath,
    stagedReleasePath,
    targetSha,
  );
  if (snapshotPath) await durableRemove(snapshotPath);
}

async function rollbackCoordinatorAfterFailure(
  location: CoordinatorDeployLocation,
  forwardError: unknown,
): Promise<never> {
  try {
    await rollbackCoordinatorDeploy(location.journalPath, location.context);
  } catch (rollbackError) {
    throw new DeployFailure(
      8,
      `${forwardError instanceof Error ? forwardError.message : String(forwardError)}\n` +
        `coordinator rollback remains incomplete: ${
          rollbackError instanceof Error ? rollbackError.message : String(rollbackError)
        }`,
    );
  }
  throw new DeployFailure(
    forwardError instanceof DeployFailure ? forwardError.exitCode : 8,
    `${forwardError instanceof Error ? forwardError.message : String(forwardError)}\n` +
      "prior coordinator service and database restored",
  );
}

export async function _handleCoordinatorInitialJournalWriteFailure(
  forwardError: unknown,
  loadJournal: () => CoordinatorDeployJournalV2 | null,
  cleanupUnjournaled: () => Promise<void>,
): Promise<never> {
  let journal: CoordinatorDeployJournalV2 | null;
  try {
    journal = loadJournal();
  } catch (loadError) {
    throw new DeployFailure(
      8,
      `${forwardError instanceof Error ? forwardError.message : String(forwardError)}\n` +
        `coordinator journal checkpoint is unreadable; release and snapshot retained: ${
          loadError instanceof Error ? loadError.message : String(loadError)
        }`,
    );
  }
  if (journal) {
    throw new DeployFailure(
      8,
      `${forwardError instanceof Error ? forwardError.message : String(forwardError)}\n` +
        "coordinator prepared journal is visible; release and snapshot retained for recovery",
    );
  }
  await cleanupUnjournaled();
  throw forwardError;
}

export async function deployLocalCoordinatorHeld(options: {
  targetSha: string;
  priorSha: string;
  rolloutId: string;
  targetWorkerFingerprints: readonly string[];
  buildWeb: boolean;
}): Promise<HeldCoordinatorDeploy> {
  const location = prepareCoordinatorDeployLocation();
  const transaction = await acquireMachineTransaction("deploy", location.journalPath);
  try {
    const serviceRoot = dirname(location.context.transactionRoot);
    const foreignJournal = foreignWorkerDeployJournalForCoordinator(serviceRoot);
    if (foreignJournal) {
      failDeploy(5, `cannot mutate past unsettled foreign worker deploy journal: ${foreignJournal}`);
    }
    if (loadCoordinatorDeployJournal(location.journalPath, location.context)) {
      failDeploy(5, "an unsettled coordinator rollout must be recovered before staging a new release");
    }
    const rollout = assertWorkerRolloutDirective({ ...options, action: "hold" });
    const { targetSha, priorSha, rolloutId } = rollout;
    const { buildWeb } = options;
    const coordinatorRepo = resolveCoordinatorRepo();
    const stagedReleasePath = join(location.context.releaseRoot, `${targetSha}-${rolloutId}`);
    const servicePath = location.context.servicePath;
    if (!existsSync(servicePath)) failDeploy(5, `coordinator service definition is missing: ${servicePath}`);

    const priorDefinition = readFileSync(servicePath);
    const priorDefinitionMode = lstatSync(servicePath).mode & 0o777;
    const priorDefinitionText = priorDefinition.toString("utf8");
    const installedEnvironment = coordinatorInstallEnvironment(
      priorDefinitionText,
      location.context.platform,
    );
    const installedSha = installedEnvironment.ROOST_GIT_SHA ?? installedEnvironment.GIT_SHA;
    if (installedSha !== priorSha) {
      failDeploy(5, `coordinator service definition does not prove fleet prior SHA ${priorSha}`);
    }
    const databasePathValue = installedEnvironment.ROOST_COORDINATOR_DB;
    if (!databasePathValue) {
      failDeploy(5, "coordinator service definition does not contain ROOST_COORDINATOR_DB");
    }
    const databasePath = resolve(databasePathValue);
    const sourcePathValue = coordinatorRepoFromService(
      priorDefinitionText,
      location.context.platform,
    );
    if (!sourcePathValue) failDeploy(5, "coordinator service definition has no WorkingDirectory");
    const sourceReleasePath = realpathSync(resolve(sourcePathValue));

    const priorReport = await statusReport();
    if (!coordinatorReportIsHealthy(priorReport, priorSha)
      || !await coordinatorStartupPolicyIsEnabled(location.context.platform)) {
      failDeploy(5, `coordinator must be running at ${priorSha} with automatic startup before update`);
    }

    console.log(`\n>> stage coordinator release ${stagedReleasePath}`);
    try {
      await runOrDie(["git", "fetch", "--quiet", "origin"], "coordinator git fetch", {
        cwd: coordinatorRepo,
        echo: true,
      });
      await runOrDie(
        ["git", "worktree", "add", "--quiet", "--force", "--detach", stagedReleasePath, targetSha],
        "coordinator worktree stage",
        { cwd: coordinatorRepo, echo: true },
      );
      await runOrDie(["bun", "install", "--frozen-lockfile"], "coordinator frozen bun install", {
        cwd: stagedReleasePath,
        echo: true,
      });
      if (buildWeb) {
        await runOrDie(["bun", "run", "build"], "coordinator SPA build", {
          cwd: join(stagedReleasePath, "apps", "web"),
          echo: true,
        });
      } else {
        preserveWebDistForNoBuild(stagedReleasePath, installedEnvironment, sourceReleasePath);
      }
      await flushCoordinatorReleaseTree(location.context.releaseRoot, stagedReleasePath, targetSha);
    } catch (error) {
      await removeUnjournaledStage(location, coordinatorRepo, stagedReleasePath, targetSha);
      throw error;
    }

    const stillPrior = await statusReport();
    if (!coordinatorReportIsHealthy(stillPrior, priorSha)) {
      await removeUnjournaledStage(location, coordinatorRepo, stagedReleasePath, targetSha);
      failDeploy(5, `coordinator stopped proving prior SHA ${priorSha} before database snapshot`);
    }
    const snapshotPath = coordinatorDatabaseSnapshotPath(
      location.context.transactionRoot,
      rolloutId,
    );
    let snapshotSha256: string;
    try {
      snapshotSha256 = (await createCoordinatorRollbackSnapshot(databasePath, snapshotPath)).sha256;
    } catch (error) {
      await removeUnjournaledStage(location, coordinatorRepo, stagedReleasePath, targetSha, snapshotPath);
      throw error;
    }

    let journal: CoordinatorDeployJournalV2;
    try {
      journal = parseCoordinatorDeployJournal(JSON.stringify({
        schemaVersion: COORDINATOR_DEPLOY_JOURNAL_SCHEMA_VERSION,
        phase: "prepared",
        rolloutId,
        targetWorkerFingerprints: canonicalCoordinatorTargetWorkers(options.targetWorkerFingerprints),
        priorDefinitionBase64: priorDefinition.toString("base64"),
        priorDefinitionMode,
        priorSha,
        targetSha,
        servicePath,
        sourceReleasePath,
        stagingRepoPath: coordinatorRepo,
        stagedReleasePath,
        databasePath,
        databaseSnapshotPath: snapshotPath,
        databaseSnapshotSha256: snapshotSha256,
      }), location.context);
      await writeCoordinatorDeployJournal(location.journalPath, journal);
    } catch (error) {
      return await _handleCoordinatorInitialJournalWriteFailure(
        error,
        () => loadCoordinatorDeployJournal(location.journalPath, location.context),
        () => removeUnjournaledStage(
          location,
          coordinatorRepo,
          stagedReleasePath,
          targetSha,
          snapshotPath,
        ),
      );
    }

    console.log("\n>> activate and hold local coordinator");
    try {
      journal = await writeCoordinatorDeployPhase(location.journalPath, journal, "activating");
      const installEnvironment = {
        ...installedEnvironment,
        GIT_SHA: targetSha,
        ROOST_GIT_SHA: targetSha,
        ROOST_WORKDIR: stagedReleasePath,
        ROOST_EXEC_BIN: "",
        ROOST_REPO_ROOT: stagedReleasePath,
        ROOST_SKIP_ENV_LOCAL: "1",
        ROOST_WEB_DIST_PATH: join(stagedReleasePath, "apps", "web", "dist"),
        ...(location.context.platform === "linux"
          ? { ROOST_COORD_UNIT: servicePath }
          : { ROOST_COORD_PLIST: servicePath }),
      };
      await runOrDie(
        ["bash", "apps/coord/scripts/install.sh", "install"],
        "coordinator install",
        { cwd: stagedReleasePath, echo: true, env: installEnvironment },
      );
      journal = await markCoordinatorFleetConverging(location.journalPath, location.context);
    } catch (error) {
      return await rollbackCoordinatorAfterFailure(location, error);
    }
    return { ...location, journal };
  } finally {
    await transaction.release();
  }
}
