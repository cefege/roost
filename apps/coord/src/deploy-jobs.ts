// Owns the in-memory DeployJob registry and the POSIX `roost deploy`
// subprocess the Connect worker-deploy handler drives.
//
// The signed Windows-update jobs that share this registry live in
// windows-update-deploy-jobs.ts (+ -record/-runtime); this file is the generic
// registry and the operator-declared coordinator URL a deploy hands the worker.

import { BoundedBus } from "./buses.ts";
import { busToAsyncIterable } from "./sse.ts";
import {
  COORDINATOR_DIAL_URL_REQUIRED_MESSAGE,
  resolveCoordinatorDialUrl,
} from "@roost/shared/coordinator-dial-url";
import { log } from "@roost/shared/log";
import { durableRemove } from "@roost/shared/durability";
import {
  WINDOWS_UPDATE_POLL_MS,
  isDeployJobId,
  windowsUpdateDeployRecordPath,
} from "./windows-update-deploy-record.ts";
import { recoverWindowsUpdateJob } from "./windows-update-deploy-jobs.ts";
import type { DeployStreamMsg } from "./deploy-job-stream.ts";
import type { WorkerUpdateOwner } from "./worker-update-owner.ts";
export type { DeployStreamMsg } from "./deploy-job-stream.ts";
export type { DeployStartResult } from "./worker-update-owner.ts";


export interface DeployJob {
  jobId: string;
  host: string;
  startedAt: number;
  updatedAt?: number;
  completedAt?: number;
  lines: string[];
  status: "running" | "done";
  exitCode?: number | null;
  error?: string;
  bus: BoundedBus<DeployStreamMsg>;
  gcTimer: Timer | null;
  windowsUpdate?: {
    workerFp: string;
    manifestUrl: string;
    signatureUrl: string;
    manifestSha256: string;
    publisherSha256: string;
    lastSequence: number;
    inFlight: boolean;
    startAccepted: boolean;
    pollTimer: Timer | null;
    deadlineTimer: Timer | null;
    lastTransportError?: string;
    durable: boolean;
    mutationTail: Promise<void>;
  };
}

export const _deployJobs = new Map<string, DeployJob>();

export const DEPLOY_JOB_TTL_MS = 20 * 60 * 1000;
export function _gcJob(jobId: string, completedAt = Date.now()): void {
  const job = _deployJobs.get(jobId);
  if (!job) return;
  clearTimeout(job.gcTimer ?? undefined);
  const expire = async (): Promise<void> => {
    const current = _deployJobs.get(jobId);
    if (current !== job) return;
    if (current.windowsUpdate?.durable) {
      try {
        await durableRemove(windowsUpdateDeployRecordPath(jobId), {
          mode: 0o600,
          privateDacl: true,
        });
      } catch (error) {
        // The record survives until this succeeds; say which one so a stuck
        // GC loop is diagnosable instead of silently rescheduling forever.
        log.warn("windows-update", "gc_record_remove_failed", {
          job_id: jobId, record: windowsUpdateDeployRecordPath(jobId),
          error: String(error),
        });
        current.gcTimer = setTimeout(() => void expire(), WINDOWS_UPDATE_POLL_MS);
        return;
      }
    }
    clearTimeout(current.windowsUpdate?.pollTimer ?? undefined);
    clearTimeout(current.windowsUpdate?.deadlineTimer ?? undefined);
    _deployJobs.delete(jobId);
  };
  job.gcTimer = setTimeout(
    () => void expire(),
    Math.max(0, completedAt + DEPLOY_JOB_TTL_MS - Date.now()),
  );
}


/** The operator-declared origin a deployed worker dials. A worker reaches it
 * from another machine, so a loopback or link-local host is refused even when
 * declared: roost never invents a substitute. */
export function resolveDeployCoordinatorUrl(
  env: Record<string, string | undefined>,
): string | null {
  const declared = resolveCoordinatorDialUrl(env);
  if (declared === null) return null;
  let hostname: string;
  try {
    hostname = new URL(declared).hostname.toLowerCase();
  } catch {
    log.warn("deploy", "coordinator_url_malformed", { url: declared });
    return null;
  }
  if (
    hostname === "localhost"
    || hostname === "127.0.0.1"
    || hostname === "[::1]"
    || hostname.endsWith(".local")
  ) {
    log.warn("deploy", "coordinator_url_unreachable", { url: declared });
    return null;
  }
  return declared;
}


export function __clearDeployJobsForTest(): void {
  for (const job of _deployJobs.values()) {
    clearTimeout(job.gcTimer ?? undefined);
    clearTimeout(job.windowsUpdate?.pollTimer ?? undefined);
    clearTimeout(job.windowsUpdate?.deadlineTimer ?? undefined);
    job.status = "done";
  }
  _deployJobs.clear();
}

export async function* deployOutput(
  jobId: string,
  signal?: AbortSignal,
  updateOwner?: WorkerUpdateOwner,
): AsyncGenerator<DeployStreamMsg> {
  if (!isDeployJobId(jobId)) {
    yield { kind: "done", exit: null, error: "unknown jobId" };
    return;
  }
  if (updateOwner?.ownsJob(jobId)) {
    yield* updateOwner.output(jobId, signal);
    return;
  }
  let job = _deployJobs.get(jobId);
  if (!job) job = await recoverWindowsUpdateJob(jobId);
  if (!job) {
    yield { kind: "done", exit: null, error: "unknown jobId" };
    return;
  }
  for (const text of job.lines) yield { kind: "line", text };
  if (job.status === "done") {
    yield { kind: "done", exit: job.exitCode ?? null, error: job.error };
    return;
  }
  for await (const message of busToAsyncIterable(job.bus, {
    signal,
    // Byte-weight the queue so a flood of long lines trips the byte cap
    // before the frame cap; text.length approximates UTF-8 cost closely
    // enough for a bound.
    sizeOf: (message) => (message.kind === "line" ? message.text.length : 32),
  })) {
    yield message;
    if (message.kind === "done") return;
  }
}
