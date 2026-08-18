// Live-job runtime for signed Windows-update deploys: turning a durable record
// into an in-memory DeployJob, the serialized mutation tail that keeps the
// record and the job in step, the terminal commit, and the START/STATUS broker
// command lane. Split out of deploy-jobs.ts (400-line cap); the record schema
// lives in windows-update-deploy-record.ts and the start/progress entry points
// in windows-update-deploy-jobs.ts.

import { Code, ConnectError } from "@connectrpc/connect";

import { BoundedBus } from "./buses.ts";
import { signal } from "@roost/shared/diag";
import { sendWindowsUpdateBroker } from "./connect/worker-send.ts";
import { _deployJobs, _gcJob, type DeployJob, type DeployStreamMsg } from "./deploy-jobs.ts";
import {
  WINDOWS_UPDATE_POLL_MS,
  WINDOWS_UPDATE_TIMEOUT_MS,
  linesWithWindowsUpdateLine,
  normalizeWindowsUpdateLine,
  persistWindowsUpdateRecord,
  persistedWindowsUpdateRecord,
  type PersistedWindowsUpdateJob,
} from "./windows-update-deploy-record.ts";

export function emitWindowsUpdateLine(job: DeployJob, text: string): void {
  const line = normalizeWindowsUpdateLine(text);
  if (!line) return;
  job.lines = linesWithWindowsUpdateLine(job.lines, line);
  job.bus.publish({ kind: "line", text: line });
}

export function enqueueWindowsUpdateMutation(job: DeployJob, operation: () => Promise<void>): Promise<void> {
  const update = job.windowsUpdate;
  if (!update) return Promise.resolve();
  const next = update.mutationTail.catch(() => undefined).then(operation);
  update.mutationTail = next;
  return next;
}

export function armWindowsUpdateDeadline(job: DeployJob, delayMs: number, error: string): void {
  const update = job.windowsUpdate;
  if (!update || job.status !== "running") return;
  clearTimeout(update.deadlineTimer ?? undefined);
  update.deadlineTimer = setTimeout(() => {
    update.deadlineTimer = null;
    void finishWindowsUpdate(job, 1, error);
  }, Math.max(0, delayMs));
}

export async function finishWindowsUpdateNow(
  job: DeployJob,
  exit: 0 | 1,
  error?: string,
  finalLine?: string,
  acceptedProgress?: { sequence: number },
  completedAt = Date.now(),
): Promise<void> {
  if (job.status === "done") return;
  const update = job.windowsUpdate;
  if (!update) return;
  const line = finalLine ? normalizeWindowsUpdateLine(finalLine) : null;
  const lines = line ? linesWithWindowsUpdateLine(job.lines, line) : job.lines;
  const normalizedError = error
    ? normalizeWindowsUpdateLine(error) ?? "Windows update failed"
    : undefined;
  if (update.durable) {
    await persistWindowsUpdateRecord(persistedWindowsUpdateRecord(job, {
      status: "done",
      updatedAt: completedAt,
      completedAt,
      lines,
      exitCode: exit,
      error: normalizedError ?? null,
      ...(acceptedProgress
        ? { startAccepted: true, lastSequence: acceptedProgress.sequence }
        : {}),
    }));
  }
  if (acceptedProgress) {
    update.startAccepted = true;
    update.lastSequence = acceptedProgress.sequence;
    update.lastTransportError = undefined;
  }
  job.lines = lines;
  job.status = "done";
  job.updatedAt = completedAt;
  job.completedAt = completedAt;
  job.exitCode = exit;
  job.error = normalizedError;
  if (update.pollTimer) {
    clearTimeout(update.pollTimer);
    update.pollTimer = null;
  }
  if (update.deadlineTimer) {
    clearTimeout(update.deadlineTimer);
    update.deadlineTimer = null;
  }
  if (line) job.bus.publish({ kind: "line", text: line });
  if (normalizedError) {
    signal("deploy.failed", {
      host: job.host,
      exit,
      reason: normalizedError,
      cooldownKey: job.host,
    });
  }
  job.bus.publish({ kind: "done", exit, error: normalizedError });
  _gcJob(job.jobId, completedAt);
}

async function finishWindowsUpdate(
  job: DeployJob,
  exit: 0 | 1,
  error?: string,
  completedAt?: number,
): Promise<void> {
  try {
    await enqueueWindowsUpdateMutation(
      job,
      () => finishWindowsUpdateNow(job, exit, error, undefined, undefined, completedAt),
    );
  } catch (persistError) {
    if (job.status !== "running") return;
    const message = persistError instanceof Error ? persistError.message : String(persistError);
    emitWindowsUpdateLine(job, `coordinator update journal unavailable; retrying terminal commit (${message})`);
    const update = job.windowsUpdate;
    if (update && !update.deadlineTimer) {
      update.deadlineTimer = setTimeout(() => {
        update.deadlineTimer = null;
        void finishWindowsUpdate(job, exit, error, completedAt);
      }, WINDOWS_UPDATE_POLL_MS);
    }
  }
}

export function createTransientRecoveredWindowsUpdateJob(jobId: string, workerFp = ""): DeployJob {
  const existing = _deployJobs.get(jobId);
  if (existing) return existing;
  const startedAt = Date.now();
  const job: DeployJob = {
    jobId,
    host: workerFp || "Windows update host",
    startedAt,
    updatedAt: startedAt,
    lines: [],
    status: "running",
    bus: new BoundedBus<DeployStreamMsg>(2048),
    gcTimer: null,
    windowsUpdate: {
      workerFp,
      manifestUrl: "",
      signatureUrl: "",
      manifestSha256: "",
      publisherSha256: "",
      lastSequence: -1,
      startAccepted: true,
      inFlight: false,
      pollTimer: null,
      deadlineTimer: null,
      durable: false,
      mutationTail: Promise.resolve(),
    },
  };
  _deployJobs.set(jobId, job);
  armWindowsUpdateDeadline(
    job,
    WINDOWS_UPDATE_TIMEOUT_MS,
    `Windows update recovery timed out after ${WINDOWS_UPDATE_TIMEOUT_MS / 1000}s`,
  );
  emitWindowsUpdateLine(job, "resumed signed Windows update from durable worker journal");
  return job;
}

export function createPersistedWindowsUpdateJob(record: PersistedWindowsUpdateJob): DeployJob {
  const existing = _deployJobs.get(record.jobId);
  if (existing) return existing;
  const job: DeployJob = {
    jobId: record.jobId,
    host: record.host,
    startedAt: record.startedAt,
    updatedAt: record.updatedAt,
    completedAt: record.completedAt ?? undefined,
    lines: [...record.lines],
    status: record.status,
    exitCode: record.exitCode,
    error: record.error ?? undefined,
    bus: new BoundedBus<DeployStreamMsg>(2048),
    gcTimer: null,
    windowsUpdate: {
      workerFp: record.workerFp,
      manifestUrl: record.manifestUrl,
      signatureUrl: record.signatureUrl,
      manifestSha256: record.manifestSha256,
      publisherSha256: record.publisherSha256,
      lastSequence: record.lastSequence,
      startAccepted: record.startAccepted,
      inFlight: false,
      pollTimer: null,
      deadlineTimer: null,
      durable: true,
      mutationTail: Promise.resolve(),
    },
  };
  _deployJobs.set(record.jobId, job);
  if (record.status === "done") {
    _gcJob(record.jobId, record.completedAt);
  } else {
    armWindowsUpdateDeadline(
      job,
      record.startedAt + WINDOWS_UPDATE_TIMEOUT_MS - Date.now(),
      `Windows update timed out after ${WINDOWS_UPDATE_TIMEOUT_MS / 1000}s`,
    );
  }
  return job;
}

export async function recoverWindowsUpdateJobFromRecord(
  record: PersistedWindowsUpdateJob,
): Promise<DeployJob> {
  const existing = _deployJobs.get(record.jobId);
  if (existing) return existing;
  const job = createPersistedWindowsUpdateJob(record);
  if (job.status !== "running") return job;
  if (record.startedAt + WINDOWS_UPDATE_TIMEOUT_MS <= Date.now()) {
    await finishWindowsUpdate(
      job,
      1,
      `Windows update timed out after ${WINDOWS_UPDATE_TIMEOUT_MS / 1000}s`,
      record.startedAt + WINDOWS_UPDATE_TIMEOUT_MS,
    );
    return job;
  }
  emitWindowsUpdateLine(job, "resumed signed Windows update from durable coordinator journal");
  const update = job.windowsUpdate!;
  void issueWindowsUpdateCommand(job, update.startAccepted ? "STATUS" : "START");
  return job;
}

export function scheduleWindowsUpdateStatus(job: DeployJob, delayMs = WINDOWS_UPDATE_POLL_MS): void {
  const update = job.windowsUpdate;
  if (!update || job.status !== "running" || update.pollTimer) return;
  update.pollTimer = setTimeout(() => {
    update.pollTimer = null;
    void issueWindowsUpdateCommand(job, update.startAccepted ? "STATUS" : "START");
  }, delayMs);
}

export async function issueWindowsUpdateCommand(job: DeployJob, action: "START" | "STATUS"): Promise<void> {
  const update = job.windowsUpdate;
  if (!update || job.status !== "running" || update.inFlight || !update.workerFp) return;
  update.inFlight = true;
  try {
    const pending = sendWindowsUpdateBroker(update.workerFp, {
      jobId: job.jobId,
      action,
      manifestUrl: action === "START" ? update.manifestUrl : undefined,
      signatureUrl: action === "START" ? update.signatureUrl : undefined,
      manifestSha256: action === "START" ? update.manifestSha256 : undefined,
      publisherSha256: action === "START" ? update.publisherSha256 : undefined,
    });
    await pending.promise;
    if (action === "START") {
      await enqueueWindowsUpdateMutation(job, async () => {
        if (job.status !== "running" || update.startAccepted) return;
        const updatedAt = Date.now();
        if (update.durable) {
          await persistWindowsUpdateRecord(persistedWindowsUpdateRecord(job, {
            updatedAt,
            startAccepted: true,
          }));
        }
        update.startAccepted = true;
        job.updatedAt = updatedAt;
      });
    }
    update.lastTransportError = undefined;
  } catch (error) {
    if (job.status !== "running") return;
    if (error instanceof ConnectError
      && error.code !== Code.Unavailable
      && error.code !== Code.DeadlineExceeded) {
      await finishWindowsUpdate(job, 1, error.rawMessage || error.message);
      return;
    }
    // START is idempotent and transport admission is not acceptance. Until a
    // worker ACK/progress is durably recorded, reconnects continue sending
    // START; only an accepted job switches recovery polling to STATUS.
    const message = error instanceof Error ? error.message : String(error);
    if (update.lastTransportError !== message) {
      update.lastTransportError = message;
      emitWindowsUpdateLine(
        job,
        `worker link or coordinator journal unavailable; waiting for persisted updater progress (${message})`,
      );
    }
  } finally {
    update.inFlight = false;
    scheduleWindowsUpdateStatus(job);
  }
}
