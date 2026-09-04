// Localhost worker source deployment, durable rollback journal, and service
// activation. The deploy router supplies exact source and optional atomic
// fleet directives; platform service writers remain owned by install.sh.

import { existsSync, lstatSync, mkdirSync, readFileSync, realpathSync, rmSync, type Stats } from "node:fs";
import { dirname, isAbsolute, join, resolve } from "node:path";
import { durableRemove, durableWriteFile, flushDurablePath } from "@roost/shared/durability";
import { acquireMachineTransaction } from "./machine-transaction.ts";
import {
  coordServicePath,
  roostServiceDir,
  workerServicePath,
} from "@roost/shared/paths";
import {
  DeployFailure,
  failDeploy,
  finishWorkerDeploy,
  POSIX_WORKER_DEPLOY_JOURNAL_PATHS,
  run,
  workerServiceIsRunning,
} from "./deploy-exec.ts";
import {
  _activateLocalWorker,
  type LocalWorkerCommandResult as CommandResult,
} from "./deploy-local-activation.ts";
import {
  _backfillEnvFromPlist,
  _resolveDeployEnvValue,
} from "./deploy-plist-env.ts";
import { linuxWorkerResourceEnvironment } from "./deploy-linux.ts";
import {
  decodeServiceSnapshot,
  localWorkerDeployJournalPath,
  localWorkerDeployStageIsConfined,
  localWorkerReleaseMatches,
  LOCAL_WORKER_DEPLOY_JOURNAL_SCHEMA_VERSION,
  normalizedMetadataPath,
  parseLocalWorkerDeployJournal,
  priorServiceIsProven,
  serviceGitSha,
  serviceSnapshotMatches,
  serviceWorkingDirectory,
  _recoverLocalWorkerDeployJournal,
  type LocalWorkerDeployConfinement,
  type LocalWorkerDeployJournal,
  type LocalWorkerDeployRecoveryDeps,
  type LocalWorkerLifecycle,
  type LocalWorkerServiceSnapshot,
} from "./local-worker-deploy-journal.ts";
import { coordinatorJournalAllowsLocalWorkerRollout } from "./local-worker-rollout-coordinator.ts";
import {
  launchdBootstrapWithRetryCmd,
  restartWorkerCmd,
  verifyWorkerCmd,
  WORKER_AGENT,
  WORKER_UNIT,
} from "./service-ctl.ts";
import {
  assertWorkerRolloutDirective,
  type WorkerRolloutDirective,
} from "./worker-deploy-rollout.ts";



function restoreCommand(
  os: "linux" | "darwin",
  servicePath: string,
  shouldRun: boolean,
): string {
  if (os === "linux") {
    const runtime = `export XDG_RUNTIME_DIR="\${XDG_RUNTIME_DIR:-/run/user/$(id -u)}";`;
    return shouldRun
      ? `${runtime} systemctl --user daemon-reload && systemctl --user restart ${WORKER_UNIT}`
      : `${runtime} systemctl --user stop ${WORKER_UNIT} 2>/dev/null || true; ` +
        `systemctl --user daemon-reload`;
  }
  if (!shouldRun) {
    return `launchctl bootout gui/$(id -u)/${WORKER_AGENT} 2>/dev/null || true`;
  }
  return launchdBootstrapWithRetryCmd(WORKER_AGENT, servicePath, { role: "worker rollback" });
}

function lstatIfPresent(path: string): Stats | null {
  try {
    return lstatSync(path);
  } catch (error) {
    if (
      error instanceof Error
      && "code" in error
      && (error as NodeJS.ErrnoException).code === "ENOENT"
    ) {
      return null;
    }
    throw error;
  }
}


function readLocalWorkerServiceSnapshot(servicePath: string): LocalWorkerServiceSnapshot | null {
  const entry = lstatIfPresent(servicePath);
  if (!entry) return null;
  if (!entry.isFile() || entry.isSymbolicLink()) {
    throw new Error("worker service definition must be a regular file");
  }
  return {
    definitionBase64: readFileSync(servicePath).toString("base64"),
    mode: entry.mode & 0o777,
  };
}

async function checkpointLocalWorkerDeployJournal(
  journalPath: string,
  journal: Readonly<LocalWorkerDeployJournal>,
  confinement: Readonly<LocalWorkerDeployConfinement>,
): Promise<void> {
  const serialized = `${JSON.stringify(journal)}\n`;
  parseLocalWorkerDeployJournal(serialized, confinement);
  await durableWriteFile(journalPath, serialized, { mode: 0o600 });
}

function readLocalWorkerDeployJournal(journalPath: string): string | null {
  const entry = lstatIfPresent(journalPath);
  if (!entry) return null;
  if (!entry.isFile() || entry.isSymbolicLink()) {
    throw new Error("worker deploy journal must be a regular file");
  }
  try {
    return readFileSync(journalPath, "utf8");
  } catch (error) {
    if (
      error instanceof Error
      && "code" in error
      && (error as NodeJS.ErrnoException).code === "ENOENT"
    ) {
      return null;
    }
    throw error;
  }
}

async function probeLocalWorkerLifecycle(
  os: "linux" | "darwin",
): Promise<LocalWorkerLifecycle> {
  const status = await run(["bash", "-lc", verifyWorkerCmd(os)], { quiet: true });
  if (status.exit === 0 && workerServiceIsRunning(status.stdout, os)) return "running";
  if (os === "darwin") return status.exit === 0 || status.exit === 1 ? "stopped" : "unknown";
  if (status.exit === 0) return "stopped";
  const active = await run(
    [
      "bash",
      "-lc",
      `export XDG_RUNTIME_DIR="\${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"; ` +
        `systemctl --user is-active ${WORKER_UNIT} 2>/dev/null`,
    ],
    { quiet: true },
  );
  return ["inactive", "failed", "unknown", "deactivating"].includes(active.stdout.trim())
    ? "stopped"
    : "unknown";
}

async function localWorkerStartupPolicyIsEnabled(os: "linux" | "darwin"): Promise<boolean> {
  if (os === "linux") {
    const result = await run([
      "bash",
      "-lc",
      `export XDG_RUNTIME_DIR="\${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"; ` +
        `systemctl --user is-enabled ${WORKER_UNIT}`,
    ], { quiet: true });
    return result.exit === 0 && result.stdout.trim() === "enabled";
  }
  const result = await run([
    "bash",
    "-lc",
    `launchctl print-disabled gui/$(id -u)`,
  ], { quiet: true });
  return result.exit === 0
    && !new RegExp(`"${WORKER_AGENT.replaceAll(".", "[.]")}"\\s*=>\\s*true`).test(result.stdout);
}

function localCoordinatorWorkingDirectory(): string | null {
  const servicePath = coordServicePath();
  if (!existsSync(servicePath)) return null;
  try {
    const workingDirectory = serviceWorkingDirectory(
      readFileSync(servicePath, "utf8"),
      process.platform as "linux" | "darwin",
    );
    if (!workingDirectory) throw new Error("missing WorkingDirectory");
    return workingDirectory;
  } catch (error) {
    throw new Error(
      `cannot prove coordinator release use before worker cleanup: ${error instanceof Error ? error.message : String(error)}`,
    );
  }
}

async function cleanupLocalWorkerStage(
  journal: Readonly<LocalWorkerDeployJournal>,
  confinement: Readonly<LocalWorkerDeployConfinement>,
): Promise<void> {
  if (
    journal.sourceRoot !== confinement.sourceRoot
    || journal.releaseRoot !== confinement.releaseRoot
    || !localWorkerDeployStageIsConfined(journal.releaseRoot, journal.stagedReleasePath)
  ) {
    throw new Error("refusing to clean an unconfined worker deploy stage");
  }
  const entry = lstatIfPresent(journal.stagedReleasePath);
  if (!entry) return;
  let removedByGit = false;
  if (entry.isDirectory() && !entry.isSymbolicLink()) {
    const canonicalRoot = realpathSync(journal.releaseRoot);
    const canonicalStage = realpathSync(journal.stagedReleasePath);
    if (
      canonicalRoot !== journal.releaseRoot
      || !localWorkerDeployStageIsConfined(canonicalRoot, canonicalStage)
    ) {
      throw new Error("refusing to clean a worker deploy stage outside its release root");
    }
    try {
      const removed = await run(
        ["git", "worktree", "remove", "--force", journal.stagedReleasePath],
        { cwd: journal.sourceRoot, quiet: true },
      );
      removedByGit = removed.exit === 0;
    } catch {
      // Recursive removal below remains confined and handles a missing source checkout.
    }
  }
  if (!removedByGit) {
    rmSync(journal.stagedReleasePath, { recursive: true, force: true });
  }
  await flushDurablePath(journal.releaseRoot);
}

async function removeManagedPriorRelease(
  sourceRepo: string,
  releaseRoot: string,
  priorWorkingDirectory: string | null,
): Promise<void> {
  if (!priorWorkingDirectory || !localWorkerDeployStageIsConfined(releaseRoot, priorWorkingDirectory)) {
    return;
  }
  const entry = lstatIfPresent(priorWorkingDirectory);
  if (!entry) return;
  if (entry) {
    if (entry.isSymbolicLink()) {
      throw new Error("refusing to remove a symlinked prior worker release");
    }
    const canonicalRoot = realpathSync(releaseRoot);
    const canonicalPrior = realpathSync(priorWorkingDirectory);
    if (
      canonicalRoot !== releaseRoot
      || !localWorkerDeployStageIsConfined(canonicalRoot, canonicalPrior)
    ) {
      throw new Error("refusing to remove a prior worker release outside its release root");
    }
  }
  if (localCoordinatorWorkingDirectory() === priorWorkingDirectory) return;
  const removed = await run(["git", "worktree", "remove", "--force", priorWorkingDirectory], {
    cwd: sourceRepo,
    quiet: true,
  });
  if (removed.exit !== 0) {
    throw new Error(
      `cannot retire prior worker release ${priorWorkingDirectory}: ${removed.stderr.trim() || `exit ${removed.exit}`}`,
    );
  }
  if (removed.exit === 0 && existsSync(releaseRoot)) await flushDurablePath(releaseRoot);
}

function createLocalWorkerDeployRecoveryDeps(
  servicePath: string,
  journalPath: string,
  confinement: Readonly<LocalWorkerDeployConfinement>,
): LocalWorkerDeployRecoveryDeps {
  return {
    readService: () => readLocalWorkerServiceSnapshot(servicePath),
    probeLifecycle: (journal) => probeLocalWorkerLifecycle(journal.os),
    restorePrior: async (journal) => {
      if (journal.priorService) {
        await durableWriteFile(
          servicePath,
          decodeServiceSnapshot(journal.priorService),
          { mode: journal.priorService.mode },
        );
      } else if (lstatIfPresent(servicePath)) {
        await durableRemove(servicePath);
      }
      const restored = await run(
        ["bash", "-lc", restoreCommand(journal.os, servicePath, journal.priorWasRunning)],
        { cwd: journal.sourceRoot, quiet: true },
      );
      if (restored.exit !== 0) {
        throw new Error(
          `rollback failed (exit ${restored.exit})\n${restored.stdout}\n${restored.stderr}`,
        );
      }
    },
    cleanupStage: (journal) => cleanupLocalWorkerStage(journal, confinement),
    commitTarget: (journal) => removeManagedPriorRelease(
      journal.sourceRoot,
      journal.releaseRoot,
      journal.priorWorkingDirectory,
    ),
    clearJournal: async () => {
      if (lstatIfPresent(journalPath)) await durableRemove(journalPath);
    },
  };
}
async function localWorkerIsRunningAtSha(
  servicePath: string,
  os: "linux" | "darwin",
  expectedSha: string,
): Promise<boolean> {
  const service = readLocalWorkerServiceSnapshot(servicePath);
  if (!service || serviceGitSha(decodeServiceSnapshot(service).toString("utf8"), os) !== expectedSha) {
    return false;
  }
  const status = await run(["bash", "-lc", verifyWorkerCmd(os)], { quiet: true });
  return status.exit === 0 && workerServiceIsRunning(status.stdout, os);
}


/** Localhost source deployment uses the same immutable stage, service
 * snapshot, activation proof, and rollback contract as remote POSIX deploys. */
export async function _deployLocal(
  host: string,
  options: {
    sourceRoot: string;
    gitSha: string;
    rollout?: WorkerRolloutDirective;
    coordinatorUrl?: string;
  },
): Promise<void> {
  const os: "linux" | "darwin" = process.platform === "linux" ? "linux" : "darwin";
  const sourceRoot = realpathSync(resolve(options.sourceRoot));
  const localGitSha = options.gitSha;
  const rollout = options.rollout ? assertWorkerRolloutDirective(options.rollout) : null;
  if (rollout && rollout.targetSha !== localGitSha.toLowerCase()) {
    failDeploy(7, "worker rollout target does not match the local deployment SHA");
  }
  if (!/^[0-9a-f]{40}(?:[0-9a-f]{24})?$/i.test(localGitSha)) {
    failDeploy(7, "a localhost deploy requires an exact clean source commit");
  }
  const service = os === "linux" ? WORKER_UNIT : WORKER_AGENT;
  const configuredServiceDir = resolve(roostServiceDir());
  mkdirSync(configuredServiceDir, { recursive: true, mode: 0o700 });
  const serviceDir = realpathSync(configuredServiceDir);
  const journalPath = localWorkerDeployJournalPath(serviceDir);
  const transactionDirectory = dirname(journalPath);
  mkdirSync(transactionDirectory, { recursive: true, mode: 0o700 });
  if (realpathSync(transactionDirectory) !== transactionDirectory) {
    throw new DeployFailure(5, "worker deploy transaction directory must not traverse a symbolic link");
  }
  const releaseRoot = join(serviceDir, "releases", "worker");
  const confinement: LocalWorkerDeployConfinement = {
    os,
    sourceRoot,
    releaseRoot,
  };
  const servicePath = workerServicePath();
  const recoveryDeps = createLocalWorkerDeployRecoveryDeps(
    servicePath,
    journalPath,
    confinement,
  );
  const transaction = await acquireMachineTransaction("deploy", journalPath);
  try {
    for (const relative of [
      POSIX_WORKER_DEPLOY_JOURNAL_PATHS.linux,
      POSIX_WORKER_DEPLOY_JOURNAL_PATHS.darwin,
      POSIX_WORKER_DEPLOY_JOURNAL_PATHS.coordinator,
    ]) {
      const foreignJournal = join(serviceDir, relative);
      if (!lstatIfPresent(foreignJournal)) continue;
      if (relative === POSIX_WORKER_DEPLOY_JOURNAL_PATHS.coordinator
        && rollout
        && coordinatorJournalAllowsLocalWorkerRollout(serviceDir, os, rollout)) {
        continue;
      }
      throw new DeployFailure(
        5,
        `cannot mutate past unsettled foreign worker deploy journal: ${foreignJournal}`,
      );
    }
    const existingJournal = readLocalWorkerDeployJournal(journalPath);
    if (existingJournal) {
      try {
        let loaded = parseLocalWorkerDeployJournal(existingJournal, confinement);
        if (loaded.rolloutId !== null && loaded.rolloutId !== rollout?.rolloutId) {
          throw new Error("another fleet rollout still owns the local worker deploy journal");
        }
        const decision = await _recoverLocalWorkerDeployJournal(
          existingJournal,
          confinement,
          recoveryDeps,
          rollout ?? undefined,
        );
        if (decision === "target-held") {
          if (rollout?.action !== "hold") {
            throw new Error("a fleet-held local worker requires its owning rollout");
          }
          if (loaded.phase === "activating") {
            loaded = { ...loaded, phase: "activated" };
            await checkpointLocalWorkerDeployJournal(journalPath, loaded, confinement);
          }
          console.log(`>> local worker target already held for fleet rollout ${rollout.rolloutId}`);
          return;
        }
        console.log(`>> recovered interrupted local worker deploy (${decision})`);
        if (rollout?.action === "finalize") {
          if (decision !== "target-committed") throw new Error("local worker target was not finalized");
          return;
        }
        if (rollout?.action === "rollback") {
          if (decision !== "prior-restored" && decision !== "prepared-cleaned") {
            throw new Error("local worker prior state was not restored");
          }
          return;
        }
      } catch (error) {
        throw new DeployFailure(
          5,
          `cannot settle local worker deploy: ${error instanceof Error ? error.message : String(error)}`,
        );
      }
    } else if (rollout?.action === "finalize" || rollout?.action === "rollback") {
      const expectedSha = rollout.action === "finalize" ? rollout.targetSha : rollout.priorSha;
      if (!await localWorkerIsRunningAtSha(servicePath, os, expectedSha)) {
        throw new DeployFailure(5, `local worker has no journal and does not prove ${expectedSha}`);
      }
      console.log(`>> local worker already ${rollout.action === "finalize" ? "finalized" : "rolled back"}`);
      return;
    }
    const bunBin = Bun.which("bun") ?? process.execPath;
    const releaseId = `${localGitSha}-${crypto.randomUUID()}`;
    const releaseDir = join(releaseRoot, releaseId);


    console.log(`>> local deploy on ${host}`);
    const { env: hostEnv, filled } = await _backfillEnvFromPlist("self");
    if (filled.length > 0) {
      console.log(`>> reused from existing service: ${filled.join(", ")}`);
    }
    const priorService = readLocalWorkerServiceSnapshot(servicePath);
    const priorText = priorService
      ? decodeServiceSnapshot(priorService).toString("utf8")
      : "";
    const priorWorkingDirectory = priorService
      ? normalizedMetadataPath(serviceWorkingDirectory(priorText, os))
      : null;
    const priorGitSha = priorService ? serviceGitSha(priorText, os) : null;
    const priorStatus = priorService
      ? await run(["bash", "-lc", verifyWorkerCmd(os)], { quiet: true })
      : { exit: 1, stdout: "", stderr: "" };
    if (priorService && os === "linux" && priorStatus.exit !== 0) {
      failDeploy(5, `cannot snapshot ${service} active state before activation`);
    }
    const priorWasRunning = priorService !== null
      && priorStatus.exit === 0
      && workerServiceIsRunning(priorStatus.stdout, os);
    if (rollout?.action === "hold"
      && (!priorWasRunning || priorGitSha?.toLowerCase() !== rollout.priorSha)) {
      failDeploy(5, `local worker does not match rollout prior SHA ${rollout.priorSha}`);
    }
    if (priorService && (!priorWasRunning || !await localWorkerStartupPolicyIsEnabled(os))) {
      failDeploy(
        5,
        "the existing local worker must be running with its normal automatic startup policy before update",
      );
    }

    const installEnv: Record<string, string> = {
      ...hostEnv,
      ...(os === "linux" ? linuxWorkerResourceEnvironment(priorText) : {}),
    };
    for (const key of ["GIT_SHA", "ROOST_GIT_SHA", "ROOST_WORKDIR", "ROOST_EXEC_BIN", "ROOST_BOOTSTRAP_TOKEN"]) {
      delete installEnv[key];
    }
    for (const key of [
      "ROOST_COORDINATOR_URL",
      "ROOST_WORKER_LABEL",
      "ROOST_REACHABLE_ADDR",
    ]) {
      const value = _resolveDeployEnvValue(
        key,
        hostEnv,
        key === "ROOST_COORDINATOR_URL" ? options.coordinatorUrl : undefined,
      );
      if (value === undefined) delete installEnv[key];
      else installEnv[key] = value;
    }
    if (process.env.ROOST_BOOTSTRAP_TOKEN) {
      installEnv.ROOST_BOOTSTRAP_TOKEN = process.env.ROOST_BOOTSTRAP_TOKEN;
    }
    if (!installEnv.ROOST_COORDINATOR_URL) {
      failDeploy(6, "ROOST_COORDINATOR_URL env var required (no prior service definition to reuse)");
    }
    installEnv.BUN_BIN = bunBin;
    installEnv.GIT_SHA = localGitSha;

    mkdirSync(releaseRoot, { recursive: true, mode: 0o700 });
    if (realpathSync(releaseRoot) !== releaseRoot) {
      failDeploy(5, "worker release root must not traverse a symbolic link");
    }
    let journal: LocalWorkerDeployJournal = {
      schemaVersion: LOCAL_WORKER_DEPLOY_JOURNAL_SCHEMA_VERSION,
      phase: "prepared",
      os,
      sourceRoot,
      releaseRoot,
      stagedReleasePath: releaseDir,
      targetSha: localGitSha,
      rolloutId: rollout?.action === "hold" ? rollout.rolloutId : null,
      priorService,
      priorWasRunning,
      priorWorkingDirectory,
      priorGitSha,
      targetService: null,
    };
    await checkpointLocalWorkerDeployJournal(journalPath, journal, confinement);
    const cleanupStage = async (): Promise<void> => {
      await recoveryDeps.cleanupStage(journal);
      await recoveryDeps.clearJournal();
    };

    console.log(`>> stage ${localGitSha.slice(0, 8)} in ${releaseDir}`);
    const stage = await run(
      ["git", "worktree", "add", "--quiet", "--force", "--detach", releaseDir, localGitSha],
      { cwd: sourceRoot, quiet: true },
    );
    if (stage.exit !== 0) {
      await cleanupStage();
      failDeploy(2, `local source snapshot failed\n${stage.stdout}\n${stage.stderr}`);
    }
    let expectedRelease: string;
    let dependencies: CommandResult;
    try {
      expectedRelease = realpathSync(releaseDir);
      if (expectedRelease !== releaseDir) {
        throw new Error("staged worker release resolved outside its journaled path");
      }
      console.log(">> frozen bun install locally");
      dependencies = await run([bunBin, "install", "--frozen-lockfile"], {
        cwd: expectedRelease,
        quiet: true,
      });
    } catch (error) {
      await cleanupStage();
      throw error;
    }
    if (dependencies.exit !== 0) {
      await cleanupStage();
      failDeploy(4, `bun install failed\n${dependencies.stdout}\n${dependencies.stderr}`);
    }

    journal = { ...journal, phase: "activating" };
    await checkpointLocalWorkerDeployJournal(journalPath, journal, confinement);
    const rollback = async (): Promise<string | null> => {
      try {
        await recoveryDeps.restorePrior(journal);
        if (await priorServiceIsProven(journal, recoveryDeps)) return null;
        return priorWasRunning
          ? "rollback service did not become healthy with its exact prior definition"
          : "rollback could not prove the prior stopped lifecycle and exact definition";
      } catch (error) {
        return `rollback failed: ${error instanceof Error ? error.message : String(error)}`;
      }
    };

    console.log(`>> activate staged ${service}`);
    const activated = await _activateLocalWorker({
      install: async () => {
        const result = await run(
          ["bash", join(expectedRelease, "apps", "worker", "scripts", "install.sh"), "install"],
          { cwd: expectedRelease, quiet: true, env: installEnv },
        );
        if (result.exit !== 0) return result;
        const targetService = readLocalWorkerServiceSnapshot(servicePath);
        if (!targetService) throw new Error("install.sh did not create a worker service definition");
        const targetDefinition = decodeServiceSnapshot(targetService).toString("utf8");
        if (!localWorkerReleaseMatches(targetDefinition, os, expectedRelease, localGitSha)) {
          throw new Error("install.sh created a worker service definition for the wrong release");
        }
        journal = { ...journal, targetService };
        await checkpointLocalWorkerDeployJournal(journalPath, journal, confinement);
        return result;
      },
      restart: () => run(["bash", "-lc", restartWorkerCmd(os)], {
        cwd: expectedRelease,
        quiet: true,
      }),
      verify: async () => {
        const result = await run(["bash", "-lc", verifyWorkerCmd(os)], { quiet: true });
        try {
          const activeService = readLocalWorkerServiceSnapshot(servicePath);
          if (
            journal.targetService
            && serviceSnapshotMatches(activeService, journal.targetService)
            && localWorkerReleaseMatches(
              decodeServiceSnapshot(journal.targetService).toString("utf8"),
              os,
              expectedRelease,
              localGitSha,
            )
          ) {
            result.stdout += `${result.stdout.endsWith("\n") ? "" : "\n"}RoostReleaseMatch=yes\n`;
          }
        } catch {
          // The missing, unreadable, or changed definition fails release verification.
        }
        return result;
      },
      rollback,
      cleanupStage,
    });
    const installOutput = `${activated.install.stdout}${activated.install.stderr}`.trim();
    if (installOutput) {
      console.log(installOutput.split("\n").map((line) => `   ${line}`).join("\n"));
    }
    journal = { ...journal, phase: "activated" };
    await checkpointLocalWorkerDeployJournal(journalPath, journal, confinement);
    if (rollout?.action === "hold") {
      console.log(`>> held ${host} v2 worker for fleet rollout ${rollout.rolloutId}`);
      return;
    }
    await recoveryDeps.commitTarget(journal);
    await recoveryDeps.clearJournal();
    finishWorkerDeploy(
      activated.verify,
      `>> done — ${host} v2 worker deployed (local)`,
      os,
    );
  } finally {
    await transaction.release();
  }
}
