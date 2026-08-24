// Signed Windows-update deploy jobs: the START admission path (deduplicated
// against both the live registry and the durable journal), worker-reported
// progress, and the resume/recovery entry points a reconnecting worker or a
// restarted coordinator drives. Split out of deploy-jobs.ts (400-line cap),
// which keeps the generic DeployJob registry and the POSIX `roost deploy`
// spawn; the durable record schema lives in windows-update-deploy-record.ts,
// the live-job runtime in windows-update-deploy-runtime.ts, and the release
// manifest preflight in windows-update-manifest.ts.

import { readdir } from "node:fs/promises";
import { join } from "node:path";

import { BoundedBus } from "./buses.ts";
import { log } from "@roost/shared/log";
import { signal } from "@roost/shared/diag";
import { coordDataDir } from "@roost/shared/paths";
import {
  _deployJobs,
  type DeployJob,
  type DeployStartResult,
  type DeployStreamMsg,
} from "./deploy-jobs.ts";
import {
  HOST_RE,
  WINDOWS_UPDATE_TIMEOUT_MS,
  WINDOWS_WORKER_FP_RE,
  httpsUrlSchema,
  isDeployJobId,
  linesWithWindowsUpdateLine,
  loadPersistedWindowsUpdate,
  normalizeWindowsUpdateLine,
  persistWindowsUpdateRecord,
  persistedWindowsUpdateRecord,
} from "./windows-update-deploy-record.ts";
import {
  armWindowsUpdateDeadline,
  createPersistedWindowsUpdateJob,
  createTransientRecoveredWindowsUpdateJob,
  emitWindowsUpdateLine,
  enqueueWindowsUpdateMutation,
  finishWindowsUpdateNow,
  issueWindowsUpdateCommand,
  recoverWindowsUpdateJobFromRecord,
  scheduleWindowsUpdateStatus,
} from "./windows-update-deploy-runtime.ts";

export interface WindowsUpdateDeployOptions {
  workerFp: string;
  manifestUrl: string;
  signatureUrl: string;
  manifestSha256: string;
  publisherSha256: string;
}

async function recoverWindowsUpdateJobFromJournal(jobId: string): Promise<DeployJob | null> {
  if (process.platform !== "win32") return null;
  // The worker journal imports Windows service-control modules; keep that
  // platform-specific dependency out of non-Windows coordinator startup.
  const {
    DurableWindowsUpdateJournalStore,
    readWindowsUpdateProgressFromJournal,
  } = await import("../../roost-cli/src/windows/windows-update-journal.ts");
  const journal = await new DurableWindowsUpdateJournalStore().load();
  if (!journal || journal.jobId !== jobId) return null;
  const job = createTransientRecoveredWindowsUpdateJob(jobId);
  for (const entry of readWindowsUpdateProgressFromJournal(journal, 0)) {
    await handleWorkerUpdateProgress("", {
      request_id: "coordinator-recovery",
      job_id: journal.jobId,
      sequence: entry.sequence,
      phase: entry.phase,
      message: entry.message,
      terminal: entry.terminal,
      success: entry.success,
      error: entry.error,
    });
  }
  return job;
}

/** deployOutput's recovery arm: a jobId with no live registry entry is either a
 *  durable coordinator record to rehydrate or, on Windows, a worker journal to
 *  replay. */
export async function recoverWindowsUpdateJob(jobId: string): Promise<DeployJob | undefined> {
  const loaded = await loadPersistedWindowsUpdate(jobId);
  if (loaded.kind === "record") return await recoverWindowsUpdateJobFromRecord(loaded.record);
  if (loaded.kind === "missing") return await recoverWindowsUpdateJobFromJournal(jobId) ?? undefined;
  return undefined;
}

let windowsUpdateStartTail = Promise.resolve();

async function withSerializedWindowsUpdateStart<T>(operation: () => Promise<T>): Promise<T> {
  const previous = windowsUpdateStartTail;
  let release!: () => void;
  windowsUpdateStartTail = new Promise<void>((resolve) => {
    release = resolve;
  });
  await previous;
  try {
    return await operation();
  } finally {
    release();
  }
}

function windowsUpdateMatches(
  job: DeployJob,
  options: WindowsUpdateDeployOptions,
): boolean {
  const update = job.windowsUpdate;
  return job.status === "running"
    && update !== undefined
    && update.workerFp === options.workerFp
    && update.manifestUrl === options.manifestUrl
    && update.signatureUrl === options.signatureUrl
    && update.manifestSha256 === options.manifestSha256.toLowerCase()
    && update.publisherSha256 === options.publisherSha256.toLowerCase();
}

async function existingWindowsUpdateFor(
  options: WindowsUpdateDeployOptions,
): Promise<{ matching?: DeployJob; conflicting?: DeployJob }> {
  for (const job of _deployJobs.values()) {
    if (job.status !== "running" || job.windowsUpdate?.workerFp !== options.workerFp) continue;
    return windowsUpdateMatches(job, options) ? { matching: job } : { conflicting: job };
  }

  const directory = join(coordDataDir(), "windows-update-jobs");
  let names: string[];
  try {
    names = await readdir(directory);
  } catch (error) {
    if (error instanceof Error && "code" in error
      && (error as NodeJS.ErrnoException).code === "ENOENT") return {};
    throw error;
  }
  for (const name of names) {
    const jobId = name.endsWith(".json") ? name.slice(0, -5) : "";
    if (!isDeployJobId(jobId)) continue;
    const loaded = await loadPersistedWindowsUpdate(jobId);
    if (loaded.kind !== "record"
      || loaded.record.status !== "running"
      || loaded.record.workerFp !== options.workerFp) continue;
    const job = await recoverWindowsUpdateJobFromRecord(loaded.record);
    return windowsUpdateMatches(job, options) ? { matching: job } : { conflicting: job };
  }
  return {};
}

export async function startWindowsUpdateDeploy(
  host: string,
  options: WindowsUpdateDeployOptions,
): Promise<DeployStartResult> {
  return await withSerializedWindowsUpdateStart(
    () => startWindowsUpdateDeploySerialized(host, options),
  );
}

async function startWindowsUpdateDeploySerialized(
  host: string,
  options: WindowsUpdateDeployOptions,
): Promise<DeployStartResult> {
  if (!HOST_RE.test(host) || host.length > 253) return { ok: false, error: "invalid host" };
  if (!WINDOWS_WORKER_FP_RE.test(options.workerFp)) {
    return { ok: false, error: "invalid worker fingerprint" };
  }
  if (!/^[a-f0-9]{64}$/i.test(options.manifestSha256)) {
    return { ok: false, error: "invalid manifest digest" };
  }
  if (options.publisherSha256 && !/^[a-f0-9]{64}$/i.test(options.publisherSha256)) {
    return { ok: false, error: "invalid Windows publisher pin" };
  }
  if (!httpsUrlSchema.safeParse(options.manifestUrl).success
    || !httpsUrlSchema.safeParse(options.signatureUrl).success) {
    return { ok: false, error: "Windows update manifest URLs must use HTTPS" };
  }
  let existing: Awaited<ReturnType<typeof existingWindowsUpdateFor>>;
  try {
    existing = await existingWindowsUpdateFor(options);
  } catch (error) {
    return {
      ok: false,
      error: `failed to inspect durable Windows update jobs: ${String(error)}`,
    };
  }
  if (existing.matching) return { ok: true, jobId: existing.matching.jobId };
  if (existing.conflicting) {
    return {
      ok: false,
      error: `Windows worker already has active update job ${existing.conflicting.jobId}`,
    };
  }

  const jobId = crypto.randomUUID();
  const startedAt = Date.now();
  const initialLine = normalizeWindowsUpdateLine(`starting signed Windows update on ${host}`)!;
  const job: DeployJob = {
    jobId,
    host,
    startedAt,
    updatedAt: startedAt,
    lines: [initialLine],
    status: "running",
    bus: new BoundedBus<DeployStreamMsg>(2048),
    gcTimer: null,
    windowsUpdate: {
      workerFp: options.workerFp,
      manifestUrl: options.manifestUrl,
      signatureUrl: options.signatureUrl,
      manifestSha256: options.manifestSha256.toLowerCase(),
      publisherSha256: options.publisherSha256.toLowerCase(),
      lastSequence: -1,
      startAccepted: false,
      inFlight: false,
      pollTimer: null,
      deadlineTimer: null,
      durable: true,
      mutationTail: Promise.resolve(),
    },
  };
  try {
    await persistWindowsUpdateRecord(persistedWindowsUpdateRecord(job));
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    return { ok: false, error: `failed to persist Windows update job: ${message}` };
  }
  _deployJobs.set(jobId, job);
  armWindowsUpdateDeadline(
    job,
    WINDOWS_UPDATE_TIMEOUT_MS,
    `Windows update timed out after ${WINDOWS_UPDATE_TIMEOUT_MS / 1000}s`,
  );
  void issueWindowsUpdateCommand(job, "START");
  return { ok: true, jobId };
}

async function jobForWindowsUpdateProgress(
  jobId: string,
  workerFp: string,
): Promise<DeployJob | null> {
  const existing = _deployJobs.get(jobId);
  if (existing) return existing;
  const loaded = await loadPersistedWindowsUpdate(jobId);
  if (loaded.kind === "record") return createPersistedWindowsUpdateJob(loaded.record);
  if (loaded.kind !== "missing") return null;
  return createTransientRecoveredWindowsUpdateJob(jobId, workerFp);
}

export async function handleWorkerUpdateProgress(workerFp: string, progress: {
  request_id: string;
  job_id: string;
  sequence: number;
  phase: string;
  message: string;
  terminal: boolean;
  success: boolean;
  error?: string;
}): Promise<void> {
  if (!isDeployJobId(progress.job_id)
    || (workerFp !== "" && !WINDOWS_WORKER_FP_RE.test(workerFp))
    || !Number.isSafeInteger(progress.sequence)
    || progress.sequence < 0
    || typeof progress.phase !== "string"
    || typeof progress.message !== "string"
    || typeof progress.terminal !== "boolean"
    || typeof progress.success !== "boolean"
    || (progress.error !== undefined && typeof progress.error !== "string")) {
    return;
  }
  let job: DeployJob | null;
  try {
    job = await jobForWindowsUpdateProgress(progress.job_id, workerFp);
  } catch (error) {
    log.warn("windows-update", "progress_job_lookup_failed", {
      job_id: progress.job_id, worker_fp: workerFp, error: String(error),
    });
    return;
  }
  if (!job) return;
  const update = job.windowsUpdate;
  if (!update) return;
  try {
    await enqueueWindowsUpdateMutation(job, async () => {
      if (job.status !== "running") return;
      if (!update.workerFp && workerFp) {
        update.workerFp = workerFp;
        job.host = workerFp;
      }
      if (update.workerFp !== workerFp && workerFp) return;
      if (progress.sequence <= update.lastSequence) {
        if (update.workerFp) scheduleWindowsUpdateStatus(job);
        return;
      }
      const detail = progress.message || progress.error || progress.phase || "update progress";
      const line = normalizeWindowsUpdateLine(`[${progress.phase || "update"}] ${detail}`)!;
      const lines = linesWithWindowsUpdateLine(job.lines, line);
      const updatedAt = Date.now();
      const terminalError = progress.success
        ? undefined
        : (progress.error || "Windows update failed");
      if (progress.terminal) {
        await finishWindowsUpdateNow(
          job,
          progress.success ? 0 : 1,
          terminalError,
          line,
          { sequence: progress.sequence },
        );
        return;
      }
      if (update.durable) {
        await persistWindowsUpdateRecord(persistedWindowsUpdateRecord(job, {
          updatedAt,
          startAccepted: true,
          lastSequence: progress.sequence,
          lines,
        }));
      }
      update.lastSequence = progress.sequence;
      update.startAccepted = true;
      update.lastTransportError = undefined;
      job.updatedAt = updatedAt;
      job.lines = lines;
      job.bus.publish({ kind: "line", text: line });
      if (update.workerFp) scheduleWindowsUpdateStatus(job);
    });
  } catch (error) {
    if (job.status !== "running") return;
    const message = error instanceof Error ? error.message : String(error);
    emitWindowsUpdateLine(job, `coordinator update journal unavailable; awaiting replay (${message})`);
    if (update.workerFp) scheduleWindowsUpdateStatus(job);
  }
}

export function resumeWindowsUpdateDeploysForWorker(workerFp: string): void {
  if (!WINDOWS_WORKER_FP_RE.test(workerFp)) return;
  for (const job of _deployJobs.values()) {
    const update = job.windowsUpdate;
    if (!update || update.workerFp !== workerFp || job.status !== "running") continue;
    if (update.pollTimer) {
      clearTimeout(update.pollTimer);
      update.pollTimer = null;
    }
    void issueWindowsUpdateCommand(job, update.startAccepted ? "STATUS" : "START");
  }
  void resumePersistedWindowsUpdateDeploysForWorker(workerFp);
}

export async function resumePersistedWindowsUpdateDeploysForWorker(workerFp: string): Promise<void> {
  if (!WINDOWS_WORKER_FP_RE.test(workerFp)) return;
  const directory = join(coordDataDir(), "windows-update-jobs");
  let names: string[];
  try {
    names = await readdir(directory);
  } catch (error) {
    if (error instanceof Error && "code" in error
      && (error as NodeJS.ErrnoException).code === "ENOENT") return;
    signal("deploy.failed", {
      host: workerFp,
      reason: `cannot scan durable Windows update jobs: ${String(error)}`,
      cooldownKey: workerFp,
    });
    return;
  }
  for (const name of names) {
    const jobId = name.endsWith(".json") ? name.slice(0, -5) : "";
    if (!isDeployJobId(jobId) || _deployJobs.has(jobId)) continue;
    const loaded = await loadPersistedWindowsUpdate(jobId);
    if (loaded.kind !== "record"
      || loaded.record.status !== "running"
      || loaded.record.workerFp !== workerFp) continue;
    await recoverWindowsUpdateJobFromRecord(loaded.record);
  }
}
