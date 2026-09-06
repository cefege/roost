// Crash-safe POSIX self-update admission, executable rollback, and recovery.
// Every keeper action is issued by a running coordinator-routable worker, then
// that worker is restarted and exactly proved before transaction cleanup.

import { createHash, randomUUID } from "node:crypto";
import { createReadStream } from "node:fs";
import { chmod, copyFile, open, rename, rm, stat } from "node:fs/promises";
import { dirname } from "node:path";
import {
  JournaledKeeperUpdateV1Schema,
  KeeperContractV1Schema,
  keeperUpdateAdmission,
  type KeeperContractV1,
} from "@roost/shared/keeper-update";
import { coordServicePath, workerServicePath } from "@roost/shared/paths";
import {
  createJournaledKeeperUpdateCallbacks,
  localUpdateWorker,
  type JournaledKeeperUpdateCallbacks,
} from "./direct-keeper-update.ts";
import {
  coordinatorRestartCommand,
  coordinatorStopCommand,
} from "./coordinator-service-definition.ts";
import {
  coordinatorReportIsHealthy,
  coordinatorStartupPolicyIsEnabled,
} from "./coordinator-deploy-recovery.ts";
import {
  startLocalWorkerForActivation,
  stopLocalWorkerForActivation,
} from "./deploy-local-activation.ts";
import { run } from "./deploy-exec.ts";
import { readLocalWorkerPriorState } from "./deploy-local-service-lifecycle.ts";
import {
  checkpointPosixSelfUpdate,
  clearPosixSelfUpdateJournal,
  loadPosixSelfUpdateJournal,
  posixSelfUpdateJournalPath,
  writePosixSelfUpdateJournal,
  type PosixSelfUpdateJournalV3,
} from "./posix-self-update-journal.ts";
import { statusReport } from "./status.ts";

const RELEASE_VERSION_RE = /^v?((?:0|[1-9]\d*)\.(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)(?:-(?:0|[1-9]\d*|[0-9A-Za-z-]*[A-Za-z-][0-9A-Za-z-]*)(?:\.(?:0|[1-9]\d*|[0-9A-Za-z-]*[A-Za-z-][0-9A-Za-z-]*))*)?)(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?$/;

export function canonicalReleaseVersion(version: string): string {
  if (version.length === 0 || version.length > 128) throw new Error("invalid release version");
  const match = RELEASE_VERSION_RE.exec(version);
  if (!match) throw new Error(`invalid release version: ${JSON.stringify(version)}`);
  return match[1]!;
}

async function readCandidateKeeperContract(path: string): Promise<KeeperContractV1> {
  const child = Bun.spawn([path, "__keeper-contract"], {
    stdin: "ignore", stdout: "pipe", stderr: "pipe",
  });
  const [exitCode, stdout, stderr] = await Promise.all([
    child.exited, new Response(child.stdout).text(), new Response(child.stderr).text(),
  ]);
  if (exitCode !== 0) throw new Error(`candidate keeper contract probe failed: ${stderr.trim()}`);
  try {
    return KeeperContractV1Schema.parse(JSON.parse(stdout));
  } catch (error) {
    throw new Error(`candidate keeper contract is malformed: ${String(error)}`);
  }
}


async function hashFile(path: string): Promise<string> {
  const hash = createHash("sha256");
  for await (const chunk of createReadStream(path)) hash.update(chunk);
  return hash.digest("hex");
}

async function syncPath(path: string): Promise<void> {
  const file = await open(path, "r");
  try { await file.sync(); } finally { await file.close(); }
}

async function retainSourceExecutable(journal: PosixSelfUpdateJournalV3): Promise<void> {
  await rm(journal.rollback_path, { force: true });
  await copyFile(journal.executable_path, journal.rollback_path);
  await chmod(journal.rollback_path, journal.source_binary_mode);
  await syncPath(journal.rollback_path);
  await syncPath(dirname(journal.rollback_path));
  if (await hashFile(journal.rollback_path) !== journal.source_binary_sha256) {
    throw new Error("self-update rollback executable did not copy exactly");
  }
}

async function restoreSourceExecutable(journal: PosixSelfUpdateJournalV3): Promise<void> {
  try {
    if (await hashFile(journal.executable_path) === journal.source_binary_sha256) return;
  } catch {
    // A missing executable still recovers from the durable rollback copy below.
  }
  if (await hashFile(journal.rollback_path) !== journal.source_binary_sha256) {
    throw new Error("self-update rollback executable is missing or corrupt");
  }
  const temporary = `${journal.executable_path}.restore-${randomUUID()}`;
  try {
    await copyFile(journal.rollback_path, temporary);
    await chmod(temporary, journal.source_binary_mode);
    await syncPath(temporary);
    await rename(temporary, journal.executable_path);
    await syncPath(dirname(journal.executable_path));
  } finally {
    await rm(temporary, { force: true });
  }
}

export interface PosixSelfUpdateRecoveryRuntime {
  callbacks: JournaledKeeperUpdateCallbacks;
  stopWorker(journal: PosixSelfUpdateJournalV3): Promise<void>;
  startWorker(journal: PosixSelfUpdateJournalV3): Promise<void>;
  stopCoordinator(journal: PosixSelfUpdateJournalV3): Promise<void>;
  startCoordinator(journal: PosixSelfUpdateJournalV3): Promise<void>;
  proveCoordinator(journal: PosixSelfUpdateJournalV3, expectedSha: string): Promise<void>;
  now(): number;
}

function defaultRecoveryRuntime(): PosixSelfUpdateRecoveryRuntime {
  const os = process.platform === "darwin" ? "darwin" : "linux";
  return {
    callbacks: createJournaledKeeperUpdateCallbacks(),
    async stopWorker(journal) {
      const result = await stopLocalWorkerForActivation(os, dirname(journal.executable_path));
      if (result.exit !== 0) throw new Error("self-update could not stop the worker");
    },
    async startWorker(journal) {
      const result = await startLocalWorkerForActivation(
        os, workerServicePath(), dirname(journal.executable_path), "self-update worker",
      );
      if (result.exit !== 0) throw new Error("self-update could not start the worker");
    },
    async stopCoordinator(journal) {
      const result = await run(
        ["bash", "-lc", coordinatorStopCommand(os)],
        { cwd: dirname(journal.executable_path), quiet: true },
      );
      if (result.exit !== 0) throw new Error("self-update could not stop the coordinator");
    },
    async startCoordinator(journal) {
      const result = await run(
        ["bash", "-lc", coordinatorRestartCommand(coordServicePath(), os)],
        { cwd: dirname(journal.executable_path), quiet: true },
      );
      if (result.exit !== 0) throw new Error("self-update could not start the coordinator");
    },
    async proveCoordinator(_journal, expectedSha) {
      for (let attempt = 0; attempt < 60; attempt += 1) {
        try {
          if (coordinatorReportIsHealthy(await statusReport(), expectedSha)) return;
        } catch {
          // Coordinator restart and its health endpoint may settle independently.
        }
        if (attempt < 59) await Bun.sleep(1_000);
      }
      throw new Error(`self-update coordinator did not converge at ${expectedSha}`);
    },
    now: Date.now,
  };
}

async function rollbackToSource(
  journal: PosixSelfUpdateJournalV3,
  runtime: PosixSelfUpdateRecoveryRuntime,
  journalPath: string,
): Promise<void> {
  const rolling = journal.phase === "rolling_back"
    ? journal
    : await checkpointPosixSelfUpdate(journal, "rolling_back", journalPath);
  await runtime.stopWorker(rolling);
  await runtime.stopCoordinator(rolling);
  await restoreSourceExecutable(rolling);
  await runtime.startCoordinator(rolling);
  await runtime.proveCoordinator(rolling, rolling.source_worker_sha);
  await runtime.startWorker(rolling);
  await runtime.callbacks.apply(rolling.worker_fingerprint, rolling.keeper_update, "source");
  await runtime.stopWorker(rolling);
  const heartbeatNotBeforeMs = runtime.now();
  await runtime.startWorker(rolling);
  await runtime.callbacks.prove(
    rolling.worker_fingerprint, rolling.keeper_update, "source",
    rolling.source_worker_sha, heartbeatNotBeforeMs,
  );
  if (await hashFile(rolling.executable_path) !== rolling.source_binary_sha256) {
    throw new Error("self-update source executable proof failed");
  }
  await rm(rolling.rollback_path, { force: true });
  await clearPosixSelfUpdateJournal(journalPath);
}

async function convergeTarget(
  journal: PosixSelfUpdateJournalV3,
  runtime: PosixSelfUpdateRecoveryRuntime,
  journalPath: string,
): Promise<void> {
  await runtime.stopWorker(journal);
  await runtime.stopCoordinator(journal);
  await runtime.startCoordinator(journal);
  await runtime.proveCoordinator(journal, journal.target_worker_sha);
  await runtime.startWorker(journal);
  await runtime.callbacks.apply(journal.worker_fingerprint, journal.keeper_update, "target");
  await runtime.stopWorker(journal);
  const heartbeatNotBeforeMs = runtime.now();
  await runtime.startWorker(journal);
  await runtime.callbacks.prove(
    journal.worker_fingerprint, journal.keeper_update, "target",
    journal.target_worker_sha, heartbeatNotBeforeMs,
  );
  if (await hashFile(journal.executable_path) !== journal.target_binary_sha256) {
    throw new Error("self-update target executable proof failed");
  }
  const committing = journal.phase === "committing"
    ? journal
    : await checkpointPosixSelfUpdate(journal, "committing", journalPath);
  await rm(committing.rollback_path, { force: true });
  await clearPosixSelfUpdateJournal(journalPath);
}

export async function _continueAfterPosixSelfUpdateRecovery<T>(
  recover: () => Promise<"none" | "prepared-cleaned" | "source-restored" | "target-committed">,
  continueUpdate: () => Promise<T>,
): Promise<T> {
  const outcome = await recover();
  if (outcome === "source-restored") {
    throw new Error(
      "the previous self-update restored the source release; refusing an automatic retry",
    );
  }
  return continueUpdate();
}

export async function recoverPosixSelfUpdateJournal(
  runtime: PosixSelfUpdateRecoveryRuntime = defaultRecoveryRuntime(),
  journalPath = posixSelfUpdateJournalPath(),
): Promise<"none" | "prepared-cleaned" | "source-restored" | "target-committed"> {
  const journal = loadPosixSelfUpdateJournal(journalPath);
  if (!journal) return "none";
  let executableSha: string;
  try {
    executableSha = await hashFile(journal.executable_path);
  } catch {
    executableSha = "missing";
  }
  if (journal.phase === "prepared") {
    if (executableSha !== journal.source_binary_sha256) {
      throw new Error("prepared self-update executable identity changed before mutation");
    }
    await rm(journal.rollback_path, { force: true });
    await clearPosixSelfUpdateJournal(journalPath);
    return "prepared-cleaned";
  }
  if (journal.phase === "committing") {
    if (executableSha !== journal.target_binary_sha256) {
      throw new Error("committed self-update executable identity changed");
    }
    await convergeTarget(journal, runtime, journalPath);
    return "target-committed";
  }
  if (executableSha === journal.target_binary_sha256 && journal.phase !== "rolling_back") {
    try {
      await convergeTarget(journal, runtime, journalPath);
      return "target-committed";
    } catch (error) {
      const latest = loadPosixSelfUpdateJournal(journalPath) ?? journal;
      if (latest.phase === "committing") throw error;
      await rollbackToSource(latest, runtime, journalPath);
      return "source-restored";
    }
  }
  await rollbackToSource(journal, runtime, journalPath);
  return "source-restored";
}

export async function admitPosixSelfUpdateCandidate(
  candidatePath: string,
  targetVersion: string,
): Promise<void> {
  const worker = await localUpdateWorker();
  if (!worker.gitSha || !worker.keeperRuntime) {
    throw new Error("self-update source worker identity is unproven");
  }
  const os = process.platform === "darwin" ? "darwin" : "linux";
  const prior = await readLocalWorkerPriorState(os, true);
  if (prior.lifecycle !== "running" || prior.startupPolicy !== "enabled") {
    throw new Error("self-update requires a running automatically-started worker");
  }
  const coordinator = await statusReport();
  if (!coordinatorReportIsHealthy(coordinator, worker.gitSha)
    || !await coordinatorStartupPolicyIsEnabled(os)) {
    throw new Error("self-update requires a running automatically-started coordinator");
  }
  const targetContract = await readCandidateKeeperContract(candidatePath);
  const admission = keeperUpdateAdmission(
    targetContract, worker.keeperRuntime, new Set(worker.coordinatorOpenSessionIds),
  );
  if (!admission) throw new Error("self-update keeper admission is blocked or unproven");
  const executablePath = process.execPath;
  const executableStat = await stat(executablePath);
  const journal: PosixSelfUpdateJournalV3 = {
    schema_version: 3,
    phase: "prepared",
    created_at_ms: Date.now(),
    target_version: canonicalReleaseVersion(targetVersion),
    executable_path: executablePath,
    rollback_path: `${executablePath}.rollback`,
    source_binary_sha256: await hashFile(executablePath),
    target_binary_sha256: await hashFile(candidatePath),
    source_binary_mode: executableStat.mode & 0o777,
    source_worker_sha: worker.gitSha,
    target_worker_sha: targetContract.build_sha,
    prior_lifecycle: "running",
    prior_startup_policy: "enabled",
    prior_coord_lifecycle: "running",
    prior_coord_startup_policy: "enabled",
    worker_fingerprint: worker.fingerprint,
    keeper_update: JournaledKeeperUpdateV1Schema.parse({
      admission,
      source_contract: worker.keeperRuntime.running_contract,
      target_contract: targetContract,
    }),
  };
  await retainSourceExecutable(journal);
  await writePosixSelfUpdateJournal(journal, posixSelfUpdateJournalPath());
  const preparing = await checkpointPosixSelfUpdate(journal, "preparing_keeper");
  const callbacks = createJournaledKeeperUpdateCallbacks();
  await callbacks.apply(worker.fingerprint, preparing.keeper_update, "target");
  await checkpointPosixSelfUpdate(preparing, "keeper_prepared");
}
