// Shared types and pure transitions for the coordinator worker-update owner.
// The stateful owner stays below the repository file cap while tests can drive
// validation and revision behavior without spawning a deployment.

import { realpath } from "node:fs/promises";
import { resolve } from "node:path";
import {
  WORKER_UPDATE_GIT_SHA_RE,
  WORKER_UPDATE_HOST_RE,
  type WorkerUpdateFailure,
  type WorkerUpdateOperation,
  type WorkerUpdateStartRequest,
  type WorkerUpdateReport,
} from "@roost/shared/worker-update-operation";
import type { PersistedDeployJob, WorkerUpdateBaseline } from "./deploy-job-record.ts";
import type { DeployStreamMsg } from "./deploy-job-stream.ts";
import type { startDeployJobRuntime } from "./deploy-job-runtime.ts";

export const MAX_ACTIVE_POSIX_JOBS = 2;
export const WORKER_UPDATE_RETRY_DELAY_MS = 10 * 60 * 1_000;
export const WORKER_UPDATE_VERIFY_TIMEOUT_MS = 120_000;
export const WORKER_UPDATE_VERIFY_POLL_MS = 1_000;

export interface DeployStartResult {
  ok: boolean;
  jobId?: string;
  error?: string;
}

export interface WorkerUpdateVerification {
  routable: boolean;
  heartbeatAtMs: number;
  gitSha: string | null;
  journalSettled: boolean;
  keeperConverged: boolean;
}

export interface WorkerUpdateOwnerDeps {
  coordinatorOrigin: string;
  coordinatorDialUrl: string;
  readBaseline(workerFp: string): Promise<WorkerUpdateBaseline>;
  readVerification(workerFp: string): Promise<WorkerUpdateVerification | null>;
  publishOperation(workerFp: string, operation: WorkerUpdateOperation): Promise<void> | void;
  workerExists(workerFp: string): Promise<boolean>;
  now?: () => number;
  sleep?: (milliseconds: number) => Promise<void>;
  workerRoutable(workerFp: string): boolean;
  startRuntime?: typeof startDeployJobRuntime;
}

export function initialWorkerUpdateOperation(input: {
  request: WorkerUpdateStartRequest;
  jobId: string;
  revision: number;
  atMs: number;
}): WorkerUpdateOperation {
  return {
    jobId: input.jobId,
    workerFp: input.request.workerFp as WorkerUpdateOperation["workerFp"],
    host: input.request.host,
    revision: input.revision,
    targetGitSha: input.request.expectedGitSha,
    source: input.request.source,
    status: "queued",
    phase: "preflight",
    reasonCode: null,
    message: "Update queued",
    createdAtMs: input.atMs,
    updatedAtMs: input.atMs,
    startedAtMs: null,
    completedAtMs: null,
    nextAttemptAtMs: null,
    exitCode: null,
  };
}

export function initialDeployJobRecord(input: {
  operation: WorkerUpdateOperation;
  sourceRoot: string;
  baseline: WorkerUpdateBaseline;
  coordinatorOrigin: string;
}): PersistedDeployJob {
  const report: WorkerUpdateReport = {
    schemaVersion: 1,
    operation: input.operation,
    observedGitSha: input.baseline.gitSha,
    coordinatorOrigin: input.coordinatorOrigin,
    failure: null,
    events: [{
      atMs: input.operation.createdAtMs,
      phase: "preflight",
      message: "Update queued",
    }],
  };
  return {
    schemaVersion: 1,
    operation: input.operation,
    report,
    sourceRoot: input.sourceRoot,
    sourceMode: "coordinator-pinned",
    baseline: input.baseline,
    lines: [],
  };
}

export function terminalDeployFrame(operation: WorkerUpdateOperation): DeployStreamMsg {
  return {
    kind: "done",
    exit: operation.exitCode,
    error: operation.status === "succeeded" ? undefined : operation.message ?? undefined,
  };
}

export function workerUpdateStatusIsTerminal(
  status: WorkerUpdateOperation["status"],
): boolean {
  return status === "blocked" || status === "succeeded" || status === "failed";
}

export async function canonicalCoordinatorSourceRoot(sourceRoot: string): Promise<string> {
  if (!sourceRoot || resolve(sourceRoot) !== sourceRoot || /[\r\n\0]/.test(sourceRoot)) {
    throw new Error("coordinator pinned source root is invalid");
  }
  if (await realpath(sourceRoot) !== sourceRoot) {
    throw new Error("coordinator pinned source root is not canonical");
  }
  return sourceRoot;
}

export function classifyWorkerUpdateFailure(failure: WorkerUpdateFailure | null): {
  status: "blocked" | "failed";
  reasonCode: WorkerUpdateOperation["reasonCode"];
} {
  switch (failure?.code) {
    case "source_unavailable":
    case "runtime_unavailable":
    case "keeper_incompatible":
    case "keeper_unproven":
    case "journal_conflict":
    case "unsupported_platform":
      return { status: "blocked", reasonCode: failure.code };
    default:
      return { status: "failed", reasonCode: "deploy_failed" };
  }
}

export function validateWorkerUpdateStart(request: {
  workerFp: string;
  host: string;
  expectedGitSha: string;
  sourceMode: string;
}): DeployStartResult {
  if (!/^[0-9a-f]{64}$/.test(request.workerFp)) return { ok: false, error: "invalid worker fingerprint" };
  if (!request.host || request.host.length > 253 || !WORKER_UPDATE_HOST_RE.test(request.host)) {
    return { ok: false, error: "invalid host" };
  }
  if (!WORKER_UPDATE_GIT_SHA_RE.test(request.expectedGitSha)) {
    return { ok: false, error: "invalid expected git SHA" };
  }
  if (request.sourceMode !== "coordinator-pinned") {
    return { ok: false, error: "invalid source mode" };
  }
  return { ok: true };
}

export function nextDeployJobRecord(
  record: PersistedDeployJob,
  patch: Partial<WorkerUpdateOperation>,
  progressMessage: string,
  atMs: number,
  failure: WorkerUpdateFailure | null = record.report.failure,
  observedGitSha: string | null = record.report.observedGitSha,
): PersistedDeployJob {
  const operation = {
    ...record.operation,
    ...patch,
    revision: record.operation.revision + 1,
    updatedAtMs: atMs,
  } as WorkerUpdateOperation;
  const report: WorkerUpdateReport = {
    ...record.report,
    operation,
    observedGitSha,
    failure,
    events: [...record.report.events, {
      atMs,
      phase: operation.phase,
      message: progressMessage,
    }].slice(-256),
  };
  return { ...record, operation, report };
}

export function finishedDeployJobRecord(input: {
  record: PersistedDeployJob;
  status: "succeeded" | "failed" | "blocked";
  reasonCode: WorkerUpdateOperation["reasonCode"];
  message: string;
  exitCode: number | null;
  atMs: number;
  observedGitSha: string | null;
  failureOverride: WorkerUpdateFailure | null;
}): PersistedDeployJob {
  const failure: WorkerUpdateFailure | null = input.status === "succeeded"
    ? null
    : input.failureOverride ?? {
        code: input.reasonCode ?? "deploy_failed",
        phase: "settled",
        message: input.message,
        journal: null,
        expectedKeeper: null,
        observedKeeper: null,
        targetContract: null,
      };
  return nextDeployJobRecord(input.record, {
    status: input.status,
    phase: "settled",
    reasonCode: input.reasonCode,
    message: input.message,
    completedAtMs: input.atMs,
    nextAttemptAtMs: null,
    exitCode: input.exitCode,
  }, input.message, input.atMs, failure, input.observedGitSha);
}

export function offlineDeployJobRecord(
  record: PersistedDeployJob,
  atMs: number,
): PersistedDeployJob {
  return nextDeployJobRecord(record, {
    status: "waiting",
    reasonCode: "offline",
    message: "Update pending while worker is offline",
    nextAttemptAtMs: atMs + WORKER_UPDATE_RETRY_DELAY_MS,
  }, "Worker is offline; update deferred", atMs);
}

export function interruptedDeployJobRecord(
  record: PersistedDeployJob,
  atMs: number,
): PersistedDeployJob {
  return nextDeployJobRecord(record, {
    status: "waiting",
    phase: "recovery",
    reasonCode: "coordinator_restarting",
    message: "Waiting for coordinator recovery",
    nextAttemptAtMs: atMs,
  }, "Coordinator shutdown interrupted the deploy", atMs);
}
