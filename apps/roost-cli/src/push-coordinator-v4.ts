// Coordinator-only V4 activation. New pushes stage and commit the coordinator
// before submitting independent worker jobs; schema-3 participant recovery
// remains isolated in the legacy rollout modules.

import {
  existsSync,
  lstatSync,
  readFileSync,
  rmSync,
} from "node:fs";
import { dirname, join, resolve } from "node:path";
import { coordServiceLabel } from "@roost/shared/paths";
import {
  durableRemove,
  durableReplace,
  flushDurablePath,
} from "@roost/shared/durability";
import type { CoordinatorDeployJournalV4 } from "@roost/shared/coordinator-deploy-state";
import { acquireMachineTransaction } from "./machine-transaction.ts";
import { DeployFailure, failDeploy, run, runOrDie } from "./deploy-exec.ts";
import { coordinatorDatabaseSnapshotPath } from "./coordinator-deploy-journal.ts";
import {
  checkpointCoordinatorDeployPhaseV4,
  parseCoordinatorDeployJournalV4,
  writeCoordinatorDeployJournalV4,
} from "./coordinator-deploy-journal-v4.ts";
import {
  coordinatorInstallEnvironment,
  coordinatorRepoFromService,
  coordinatorRestartCommand,
  coordinatorStopCommand,
} from "./coordinator-service-definition.ts";
import {
  flushCoordinatorReleaseTree,
  removeStagedCoordinatorRelease,
  retirePriorCoordinatorRelease,
} from "./coordinator-deploy-release.ts";
import { createCoordinatorRollbackSnapshot } from "./coordinator-deploy-snapshot.ts";
import {
  prepareCoordinatorDeployLocation,
  preserveWebDistForNoBuild,
  resolveCoordinatorRepo,
  type CoordinatorDeployLocation,
} from "./push-coordinator-location.ts";
import { statusReport } from "./status.ts";
import { POSIX_FULL_GIT_SHA_RE } from "./posix-deploy-journal.ts";
import { recoverCoordinatorDeployV4 } from "./coordinator-deploy-recovery-v4.ts";

const READINESS_TIMEOUT_MS = 60_000;
const READINESS_POLL_MS = 250;

export interface CommittedCoordinatorDeploy extends CoordinatorDeployLocation {
  journal: CoordinatorDeployJournalV4;
}

export function _coordinatorCandidateDefinitionPath(
  servicePath: string,
  rolloutId: string,
  platform: CoordinatorDeployLocation["context"]["platform"],
): string {
  const extension = platform === "linux" ? ".service" : ".plist";
  const basePath = servicePath.endsWith(extension)
    ? servicePath.slice(0, -extension.length)
    : servicePath;
  return `${basePath}.candidate-${rolloutId}${extension}`;
}

export async function deployLocalCoordinator(options: {
  targetSha: string;
  rolloutId: string;
  buildWeb: boolean;
}): Promise<CommittedCoordinatorDeploy> {
  const location = prepareCoordinatorDeployLocation();
  const transaction = await acquireMachineTransaction("deploy", location.journalPath);
  try {
    if (existsSync(location.journalPath)) {
      failDeploy(5, "unsettled coordinator rollout must recover before a new activation");
    }
    const targetSha = options.targetSha.toLowerCase();
    if (!POSIX_FULL_GIT_SHA_RE.test(targetSha)) {
      failDeploy(7, "coordinator target must be a full Git SHA");
    }
    const servicePath = location.context.servicePath;
    if (!existsSync(servicePath)) failDeploy(5, `coordinator service definition is missing: ${servicePath}`);
    const priorDefinition = readFileSync(servicePath);
    const priorDefinitionMode = lstatSync(servicePath).mode & 0o777;
    const priorText = priorDefinition.toString("utf8");
    const installedEnvironment = coordinatorInstallEnvironment(
      priorText,
      location.context.platform,
    );
    const priorSha = installedEnvironment.ROOST_GIT_SHA ?? installedEnvironment.GIT_SHA;
    if (!priorSha || !POSIX_FULL_GIT_SHA_RE.test(priorSha)) {
      failDeploy(5, "coordinator service definition does not prove its prior SHA");
    }
    const databasePathValue = installedEnvironment.ROOST_COORDINATOR_DB;
    if (!databasePathValue) failDeploy(5, "coordinator service definition has no database path");
    const databasePath = resolve(databasePathValue);
    const sourceValue = coordinatorRepoFromService(priorText, location.context.platform);
    if (!sourceValue) failDeploy(5, "coordinator service definition has no source root");
    const sourceReleasePath = resolve(sourceValue);
    const stagingRepoPath = resolveCoordinatorRepo();
    const stagedReleasePath = join(
      location.context.releaseRoot,
      `${targetSha}-${options.rolloutId}`,
    );
    const snapshotPath = coordinatorDatabaseSnapshotPath(
      location.context.transactionRoot,
      options.rolloutId,
    );

    await stageCoordinatorRelease({
      stagingRepoPath,
      stagedReleasePath,
      targetSha,
      buildWeb: options.buildWeb,
      installedEnvironment,
      sourceReleasePath,
      releaseRoot: location.context.releaseRoot,
    });
    let journal = parseCoordinatorDeployJournalV4(JSON.stringify({
      schemaVersion: 4,
      phase: "prepared",
      preparedAtMs: Date.now(),
      rolloutId: options.rolloutId,
      priorDefinitionBase64: priorDefinition.toString("base64"),
      priorDefinitionMode,
      priorSha,
      targetSha,
      servicePath,
      sourceReleasePath,
      stagingRepoPath,
      stagedReleasePath,
      databasePath,
      databaseSnapshotPath: snapshotPath,
      databaseSnapshotSha256: null,
    }), location.context);
    await writeCoordinatorDeployJournalV4(location.journalPath, journal);
    journal = await checkpointCoordinatorDeployPhaseV4(
      location.journalPath,
      journal,
      "snapshotting",
    );

    try {
      await stopCoordinatorAndProve(location, journal);
      const snapshot = await createCoordinatorRollbackSnapshot(databasePath, snapshotPath);
      journal = await checkpointCoordinatorDeployPhaseV4(
        location.journalPath,
        journal,
        "activating",
        { databaseSnapshotSha256: snapshot.sha256 },
      );
      await installCandidateDefinition(location, journal, installedEnvironment);
      await startCoordinator(location, journal);
      await waitForUpdateIdentity(journal, false);
      journal = await checkpointCoordinatorDeployPhaseV4(
        location.journalPath,
        journal,
        "finalizing",
      );
      await waitForUpdateIdentity(journal, true);
      await flushDurablePath(journal.servicePath);
      await retirePriorCoordinatorRelease(
        location.context.releaseRoot,
        journal,
        location.context.platform,
      );
      await durableRemove(journal.databaseSnapshotPath);
      await durableRemove(location.journalPath);
      return { ...location, journal };
    } catch (error) {
      const recovery = await recoverCoordinatorDeployV4(
        location.journalPath,
        location.context,
      );
      const recoveryMessage = recovery === "target-finalized"
        ? "target coordinator remains committed"
        : recovery === "prior-restored"
          ? "prior coordinator restored"
          : recovery === "prepared-cleaned"
            ? "unactivated coordinator stage removed"
            : "coordinator recovery evidence retained";
      throw new DeployFailure(
        error instanceof DeployFailure ? error.exitCode : 8,
        `${error instanceof Error ? error.message : String(error)}\n${recoveryMessage}`,
      );
    }
  } finally {
    await transaction.release();
  }
}

async function stageCoordinatorRelease(input: {
  stagingRepoPath: string;
  stagedReleasePath: string;
  targetSha: string;
  buildWeb: boolean;
  installedEnvironment: Record<string, string>;
  sourceReleasePath: string;
  releaseRoot: string;
}): Promise<void> {
  try {
    await runOrDie(["git", "fetch", "--quiet", "origin"], "coordinator git fetch", {
      cwd: input.stagingRepoPath,
    });
    await runOrDie([
      "git", "worktree", "add", "--quiet", "--force", "--detach",
      input.stagedReleasePath, input.targetSha,
    ], "coordinator worktree stage", { cwd: input.stagingRepoPath });
    await runOrDie(["bun", "install", "--frozen-lockfile"], "coordinator dependency install", {
      cwd: input.stagedReleasePath,
    });
    if (input.buildWeb) {
      await runOrDie(["bun", "run", "build"], "coordinator SPA build", {
        cwd: join(input.stagedReleasePath, "apps", "web"),
      });
    } else {
      preserveWebDistForNoBuild(
        input.stagedReleasePath,
        input.installedEnvironment,
        input.sourceReleasePath,
      );
    }
    await flushCoordinatorReleaseTree(
      input.releaseRoot,
      input.stagedReleasePath,
      input.targetSha,
    );
  } catch (error) {
    await removeStagedCoordinatorRelease(
      input.releaseRoot,
      input.stagingRepoPath,
      input.stagedReleasePath,
      input.targetSha,
    );
    throw error;
  }
}

async function stopCoordinatorAndProve(
  location: CoordinatorDeployLocation,
  journal: CoordinatorDeployJournalV4,
): Promise<void> {
  const stopped = await run(["bash", "-lc", coordinatorStopCommand(
    location.context.platform,
    coordServiceLabel(process.env, location.context.platform),
  )], { quiet: true });
  if (stopped.exit !== 0) failDeploy(8, `coordinator stop failed (exit ${stopped.exit})`);
  const deadline = Date.now() + READINESS_TIMEOUT_MS;
  while ((await statusReport()).coord.reachable) {
    if (Date.now() >= deadline) failDeploy(8, "prior coordinator did not stop before snapshot");
    await Bun.sleep(READINESS_POLL_MS);
  }
  if (journal.phase !== "snapshotting") failDeploy(8, "snapshotting journal was lost");
}

async function installCandidateDefinition(
  location: CoordinatorDeployLocation,
  journal: CoordinatorDeployJournalV4,
  installedEnvironment: Record<string, string>,
): Promise<void> {
  const candidatePath = _coordinatorCandidateDefinitionPath(
    journal.servicePath,
    journal.rolloutId,
    location.context.platform,
  );
  const environment = {
    ...installedEnvironment,
    GIT_SHA: journal.targetSha,
    ROOST_GIT_SHA: journal.targetSha,
    ROOST_WORKDIR: journal.stagedReleasePath,
    ROOST_EXEC_BIN: "",
    ROOST_REPO_ROOT: journal.stagedReleasePath,
    ROOST_SKIP_ENV_LOCAL: "1",
    ROOST_WEB_DIST_PATH: join(journal.stagedReleasePath, "apps", "web", "dist"),
    ...(location.context.platform === "linux"
      ? { ROOST_COORD_UNIT: candidatePath }
      : { ROOST_COORD_PLIST: candidatePath }),
  };
  try {
    await runOrDie(
      ["bash", "apps/coord/scripts/install.sh", "write-plist"],
      "render coordinator service definition",
      { cwd: journal.stagedReleasePath, env: environment },
    );
    const candidate = readFileSync(candidatePath, "utf8");
    const parsedRoot = coordinatorRepoFromService(candidate, location.context.platform);
    const parsedEnvironment = coordinatorInstallEnvironment(candidate, location.context.platform);
    if (parsedRoot !== journal.stagedReleasePath
      || (parsedEnvironment.ROOST_GIT_SHA ?? parsedEnvironment.GIT_SHA) !== journal.targetSha
      || parsedEnvironment.ROOST_COORDINATOR_DB !== journal.databasePath) {
      failDeploy(8, "candidate coordinator service definition identity is invalid");
    }
    const validation = location.context.platform === "linux"
      ? await run(["systemd-analyze", "--user", "verify", candidatePath], { quiet: true })
      : await run(["plutil", "-lint", candidatePath], { quiet: true });
    if (validation.exit !== 0) failDeploy(8, "candidate coordinator service definition is invalid");
    await durableReplace(candidatePath, journal.servicePath, {
      mode: journal.priorDefinitionMode,
    });
  } finally {
    rmSync(candidatePath, { force: true });
  }
}

async function startCoordinator(
  location: CoordinatorDeployLocation,
  journal: CoordinatorDeployJournalV4,
): Promise<void> {
  const started = await run(["bash", "-lc", coordinatorRestartCommand(
    journal.servicePath,
    location.context.platform,
    coordServiceLabel(process.env, location.context.platform),
  )], { quiet: true });
  if (started.exit !== 0) failDeploy(8, `coordinator target start failed (exit ${started.exit})`);
}

async function waitForUpdateIdentity(
  journal: CoordinatorDeployJournalV4,
  ready: boolean,
): Promise<void> {
  const deadline = Date.now() + READINESS_TIMEOUT_MS;
  for (;;) {
    const report = await statusReport();
    if (report.coord.reachable
      && report.coord.gitSha === journal.targetSha
      && report.coord.updateTransactionId === journal.rolloutId
      && report.coord.updateReady === ready) return;
    if (Date.now() >= deadline) {
      failDeploy(8, `coordinator target did not report update_ready=${String(ready)}`);
    }
    await Bun.sleep(READINESS_POLL_MS);
  }
}
