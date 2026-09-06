// Localhost worker source deployment, durable rollback journal, and service
// activation. The deploy router supplies exact source and optional atomic
// fleet directives; platform service writers remain owned by install.sh.

import { mkdirSync, realpathSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { JournaledKeeperUpdateV1Schema } from "@roost/shared/keeper-update";
import type { JournaledKeeperUpdateV1 } from "@roost/shared/keeper-update";
import { roostServiceDir, workerServicePath } from "@roost/shared/paths";
import {
  checkpointLocalWorkerDeployJournal,
  createLocalWorkerDeployRecoveryDeps,
  readLocalWorkerServiceSnapshot,
  settleLocalWorkerDeployJournal,
} from "./deploy-local-journal-runtime.ts";
import {
  _activateLocalWorker,
  startLocalWorkerForActivation,
  stopLocalWorkerForActivation,
} from "./deploy-local-activation.ts";
import type {
  LocalWorkerCommandResult as CommandResult,
} from "./deploy-local-activation.ts";
import {
  DeployFailure,
  failDeploy,
  finishWorkerDeploy,
  run,
} from "./deploy-exec.ts";
import { linuxWorkerResourceEnvironment } from "./linux-deploy-journal-commands.ts";
import { _backfillEnvFromPlist, _resolveDeployEnvValue } from "./deploy-plist-env.ts";
import { readLocalWorkerPriorState } from "./deploy-local-service-lifecycle.ts";
import {
  _recoverLocalWorkerDeployJournal,
  _rollbackLocalWorkerDeployJournal,
  decodeServiceSnapshot,
  localWorkerDeployJournalPath,
  localWorkerReleaseMatches,
  LOCAL_WORKER_DEPLOY_JOURNAL_SCHEMA_VERSION,
  normalizedMetadataPath,
  serviceGitSha,
  serviceSnapshotMatches,
  serviceWorkingDirectory,
} from "./local-worker-deploy-journal.ts";
import type {
  LocalWorkerDeployConfinement,
  LocalWorkerDeployJournal,
} from "./local-worker-deploy-journal.ts";
import { acquireMachineTransaction } from "./machine-transaction.ts";
import { verifyWorkerCmd, WORKER_AGENT, WORKER_UNIT } from "./service-ctl.ts";
import { assertWorkerRolloutDirective } from "./worker-deploy-rollout.ts";
import type { WorkerRolloutDirective } from "./worker-deploy-rollout.ts";
import type {
  DirectKeeperAdmission,
  JournaledKeeperUpdateCallbacks,
} from "./direct-keeper-update.ts";

/** Localhost source deployment uses the same immutable stage, service
 * snapshot, activation proof, and rollback contract as remote POSIX deploys. */
export async function _deployLocal(
  host: string,
  options: {
    sourceRoot: string;
    gitSha: string;
    rollout?: WorkerRolloutDirective;
    coordinatorUrl?: string;
    keeperUpdate?: JournaledKeeperUpdateV1 | null;
    workerFingerprint: string | null;
    resolveKeeperAdmission?: () => Promise<DirectKeeperAdmission | null>;
    keeperCallbacks: JournaledKeeperUpdateCallbacks;
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
    options.keeperCallbacks,
  );
  const transaction = await acquireMachineTransaction("deploy", journalPath);
  try {
    if (await settleLocalWorkerDeployJournal({
      serviceDir,
      journalPath,
      confinement,
      recoveryDeps,
      rollout,
      servicePath,
      os,
    })) {
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
    const priorState = await readLocalWorkerPriorState(os, priorService !== null);
    if (rollout?.action === "hold"
      && (priorState.lifecycle !== "running"
        || priorGitSha?.toLowerCase() !== rollout.priorSha)) {
      failDeploy(5, `local worker does not match rollout prior SHA ${rollout.priorSha}`);
    }
    if (priorService
      && (priorState.lifecycle !== "running" || priorState.startupPolicy !== "enabled")) {
      failDeploy(
        5,
        "the existing local worker must be running with its normal automatic startup policy before update",
      );
    }
    if (priorService && !priorGitSha) {
      failDeploy(5, "the existing local worker service does not prove its build identity");
    }
    const resolvedAdmission = options.resolveKeeperAdmission
      ? await options.resolveKeeperAdmission()
      : {
          keeperUpdate: options.keeperUpdate ?? null,
          workerFingerprint: options.workerFingerprint,
        };
    let suppliedKeeperUpdate: JournaledKeeperUpdateV1 | null = null;
    try {
      suppliedKeeperUpdate = resolvedAdmission?.keeperUpdate == null
        ? null
        : JournaledKeeperUpdateV1Schema.parse(resolvedAdmission.keeperUpdate);
    } catch (error) {
      failDeploy(5, `local worker keeper update proof is invalid: ${String(error)}`);
    }
    if (rollout && suppliedKeeperUpdate
      && JSON.stringify(suppliedKeeperUpdate) !== JSON.stringify(rollout.keeperUpdate)) {
      failDeploy(5, "local worker keeper update does not match its rollout directive");
    }
    const suppliedWorkerFingerprint = resolvedAdmission?.workerFingerprint ?? null;
    const workerFingerprint = rollout?.workerFingerprint ?? suppliedWorkerFingerprint;
    if (rollout && suppliedWorkerFingerprint !== rollout.workerFingerprint) {
      failDeploy(5, "local worker fingerprint does not match its rollout directive");
    }
    const keeperUpdate = rollout?.keeperUpdate ?? suppliedKeeperUpdate;
    if ((priorService === null) !== (keeperUpdate === null)
      || (keeperUpdate === null) !== (workerFingerprint === null)) {
      failDeploy(
        5,
        "local worker keeper update and fingerprint must be absent exactly when no prior service is installed",
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
      workerFingerprint,
      keeperUpdate,
      priorService,
      priorLifecycle: priorState.lifecycle,
      priorStartupPolicy: priorState.startupPolicy,
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
        await _rollbackLocalWorkerDeployJournal(journal, recoveryDeps);
        return null;
      } catch (error) {
        return `rollback failed: ${error instanceof Error ? error.message : String(error)}`;
      }
    };

    console.log(`>> activate staged ${service}`);
    const activated = await _activateLocalWorker({
      install: async () => {
        const result = await run(
          ["bash", join(expectedRelease, "apps", "worker", "scripts", "install.sh"), "write-plist"],
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
      stop: () => stopLocalWorkerForActivation(os, expectedRelease),
      keeperUpdate: journal.keeperUpdate,
      workerFingerprint: journal.workerFingerprint,
      applyKeeperUpdate: options.keeperCallbacks.apply,
      restart: () => priorService
        ? startLocalWorkerForActivation(os, servicePath, expectedRelease, "worker target")
        : run(
            ["bash", join(expectedRelease, "apps", "worker", "scripts", "install.sh"), "install"],
            { cwd: expectedRelease, quiet: true, env: installEnv },
          ),
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
    const recoveryDecision = await _recoverLocalWorkerDeployJournal(
      JSON.stringify(journal),
      confinement,
      recoveryDeps,
      rollout ?? undefined,
    );
    if (recoveryDecision === "prior-restored") {
      failDeploy(5, "target keeper convergence failed; the prior worker was restored");
    }
    if (rollout?.action === "hold") {
      console.log(`>> held ${host} v2 worker for fleet rollout ${rollout.rolloutId}`);
      return;
    }
    finishWorkerDeploy(
      activated.verify,
      `>> done — ${host} v2 worker deployed (local)`,
      os,
    );
  } finally {
    await transaction.release();
  }
}
