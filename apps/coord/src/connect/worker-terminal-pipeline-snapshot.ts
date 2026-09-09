// Typed, bounded terminal-pipeline evidence requests over routable worker links.
// DiagSnapshot supplies durable session-scoped targets; this owner binds a
// request and reply to the authenticated worker generation before formatting it.
// It never sends the generic browser-command diagnostic envelope.

import { create, toBinary } from "@bufbuild/protobuf";
import { Code, ConnectError } from "@connectrpc/connect";
import {
  CoordWorkerDownSchema,
  DTerminalPipelineSnapshotRequestSchema,
  WTerminalPipelineSnapshotSchema,
  type WTerminalPipelineSnapshot,
} from "@roost/shared/proto/worker_transport_pb";
import {
  TerminalPipelineReason,
  TerminalPipelineStage,
  TerminalPipelineTargetSchema,
} from "@roost/shared/proto/wire_pb";
import {
  createPendingRpc,
  rejectPendingRpcUnavailable,
} from "../router/pending-rpcs.ts";
import { currentRoutableWorker } from "./worker-send-target.ts";

export const TERMINAL_PIPELINE_DIAG_MAX_TARGETS = 64;

const TERMINAL_PIPELINE_DIAG_TIMEOUT_MS = 2_000;
const TERMINAL_PIPELINE_MAX_RESPONSE_BYTES = 64 * 1024;
const TERMINAL_PIPELINE_MAX_REQUEST_ID_BYTES = 256;
const TERMINAL_PIPELINE_MAX_IDENTIFIER_BYTES = 512;
const TERMINAL_PIPELINE_MAX_STAGES_PER_SESSION = 16;
const TERMINAL_PIPELINE_MAX_HISTOGRAM_BUCKETS = 16;
const UINT32_MAX = 0xffff_ffff;
const UINT64_MAX = (1n << 64n) - 1n;

export interface TerminalPipelineDiagnosticTarget {
  sessionId: string;
  viewId: string;
}

export type WorkerTerminalPipelineSnapshotErrorCode =
  | "offline"
  | "timeout"
  | "send_failed"
  | "rpc_error";

export type WorkerTerminalPipelineSnapshotResult =
  | {
      status: "ok";
      response_ms: number;
      snapshot: WTerminalPipelineSnapshot;
    }
  | {
      status: "error";
      response_ms: number;
      error: {
        code: WorkerTerminalPipelineSnapshotErrorCode;
        message: string;
      };
    };

interface TerminalPipelineDiagnosticStage {
  stage: number;
  reason: number;
  generation: string;
  stream_id: string;
  sequence: string;
  queue_frames: string;
  queue_bytes: string;
  native_buffered_bytes: string;
  oldest_age_ms: string;
  count: string;
  histogram_buckets: string[];
}

interface TerminalPipelineDiagnosticSession {
  session_id: string;
  view_id: string;
  stages: TerminalPipelineDiagnosticStage[];
}

export interface TerminalPipelineDiagnosticSnapshot {
  request_id: string;
  dropped_targets: number;
  dropped_records: number;
  sessions: TerminalPipelineDiagnosticSession[];
}

function pipelineError(
  startedAtMs: number,
  code: WorkerTerminalPipelineSnapshotErrorCode,
  message: string,
): WorkerTerminalPipelineSnapshotResult {
  return {
    status: "error",
    response_ms: Math.max(0, Date.now() - startedAtMs),
    error: { code, message: message.slice(0, 240) },
  };
}

/**
 * Routes one session-scoped sample directly to the current authenticated worker
 * generation. A later generation cannot settle this pending entry because the
 * response dispatcher includes the authenticated worker fingerprint on resolve.
 */
async function requestWorkerTerminalPipelineSnapshot(
  workerFp: string,
  targets: readonly TerminalPipelineDiagnosticTarget[],
  timeoutMs: number,
): Promise<WorkerTerminalPipelineSnapshotResult> {
  const startedAtMs = Date.now();
  const worker = currentRoutableWorker(workerFp);
  if (!worker) {
    return pipelineError(startedAtMs, "offline", "worker is not connected");
  }

  const pending = createPendingRpc<WTerminalPipelineSnapshot>(timeoutMs, workerFp);
  try {
    // Capture only the generation current at send time. The frame dispatcher
    // fences an old generation before it can resolve this worker-keyed pending RPC.
    if (currentRoutableWorker(workerFp) !== worker) {
      rejectPendingRpcUnavailable(
        pending.request_id,
        "worker connection changed before terminal pipeline request",
        workerFp,
      );
      await pending.promise.catch(() => undefined);
      return pipelineError(startedAtMs, "offline", "worker connection changed before request");
    }
    const sent = worker.send(create(CoordWorkerDownSchema, {
      frame: {
        case: "terminalPipelineSnapshot",
        value: create(DTerminalPipelineSnapshotRequestSchema, {
          requestId: pending.request_id,
          targets: targets.map((target) => create(TerminalPipelineTargetSchema, target)),
        }),
      },
    }));
    if (sent === 0) {
      rejectPendingRpcUnavailable(
        pending.request_id,
        "worker transport dropped terminal pipeline request",
        workerFp,
      );
      await pending.promise.catch(() => undefined);
      return pipelineError(startedAtMs, "send_failed", "worker transport dropped request");
    }
  } catch (error) {
    rejectPendingRpcUnavailable(
      pending.request_id,
      error instanceof Error ? error.message : "worker transport failed terminal pipeline request",
      workerFp,
    );
    await pending.promise.catch(() => undefined);
    return pipelineError(
      startedAtMs,
      "send_failed",
      error instanceof Error ? error.message : "worker transport send failed",
    );
  }

  try {
    const snapshot = await pending.promise;
    if (!terminalPipelineSnapshotMatchesRequest(snapshot, pending.request_id, targets)) {
      return pipelineError(startedAtMs, "rpc_error", "worker returned an invalid terminal pipeline snapshot");
    }
    return {
      status: "ok",
      response_ms: Math.max(0, Date.now() - startedAtMs),
      snapshot,
    };
  } catch (error) {
    const code = error instanceof ConnectError
      ? error.code === Code.DeadlineExceeded
        ? "timeout"
        : error.code === Code.Unavailable
          ? "offline"
          : "rpc_error"
      : "rpc_error";
    return pipelineError(
      startedAtMs,
      code,
      error instanceof Error ? error.message : String(error),
    );
  }
}

/**
 * Issues at most one bounded typed request per supplied worker. Target lists
 * come from durable session-scoped rows; empty groups make no worker request.
 */
export async function collectWorkerTerminalPipelineSnapshots(
  targetsByWorker: ReadonlyMap<string, readonly TerminalPipelineDiagnosticTarget[]>,
  timeoutMs = TERMINAL_PIPELINE_DIAG_TIMEOUT_MS,
): Promise<Record<string, WorkerTerminalPipelineSnapshotResult>> {
  const boundedTimeoutMs = Number.isFinite(timeoutMs)
    ? Math.max(1, Math.min(timeoutMs, 10_000))
    : TERMINAL_PIPELINE_DIAG_TIMEOUT_MS;
  const requests = [...targetsByWorker]
    .map(([workerFp, targets]) => [
      workerFp,
      normalizeTerminalPipelineDiagnosticTargets(targets),
    ] as const)
    .filter(([, targets]) => targets.length !== 0)
    .sort(([left], [right]) => left.localeCompare(right));
  const startedAtMs = Date.now();
  const settled = await Promise.allSettled(
    requests.map(([workerFp, targets]) =>
      requestWorkerTerminalPipelineSnapshot(workerFp, targets, boundedTimeoutMs)),
  );
  return Object.fromEntries(requests.map(([workerFp], index) => {
    const result = settled[index]!;
    return [
      workerFp,
      result.status === "fulfilled"
        ? result.value
        : pipelineError(
          startedAtMs,
          "rpc_error",
          result.reason instanceof Error ? result.reason.message : String(result.reason),
        ),
    ] as const;
  }));
}

/** The worker-frame dispatcher uses this before resolving any pending RPC. */
export function isTerminalPipelineSnapshotWireShape(
  value: unknown,
): value is WTerminalPipelineSnapshot {
  if (value === null || typeof value !== "object") return false;
  const snapshot = value as {
    requestId?: unknown;
    sessions?: unknown;
    droppedTargets?: unknown;
    droppedRecords?: unknown;
  };
  if (!isBoundedText(snapshot.requestId, TERMINAL_PIPELINE_MAX_REQUEST_ID_BYTES)) return false;
  if (!Array.isArray(snapshot.sessions) || snapshot.sessions.length > TERMINAL_PIPELINE_DIAG_MAX_TARGETS) return false;
  if (!isUint32(snapshot.droppedTargets) || !isUint32(snapshot.droppedRecords)) return false;

  const sessionKeys = new Set<string>();
  for (const value of snapshot.sessions) {
    if (value === null || typeof value !== "object") return false;
    const session = value as {
      sessionId?: unknown;
      viewId?: unknown;
      stages?: unknown;
    };
    if (!isBoundedText(session.sessionId, TERMINAL_PIPELINE_MAX_IDENTIFIER_BYTES)) return false;
    if (!isBoundedText(session.viewId, TERMINAL_PIPELINE_MAX_IDENTIFIER_BYTES)) return false;
    if (!Array.isArray(session.stages) || session.stages.length > TERMINAL_PIPELINE_MAX_STAGES_PER_SESSION) return false;
    const sessionKey = targetKey(session.sessionId, session.viewId);
    if (sessionKeys.has(sessionKey)) return false;
    sessionKeys.add(sessionKey);
    if (!session.stages.every(isTerminalPipelineStageShape)) return false;
  }

  try {
    return toBinary(WTerminalPipelineSnapshotSchema, value as WTerminalPipelineSnapshot).byteLength
      <= TERMINAL_PIPELINE_MAX_RESPONSE_BYTES;
  } catch {
    return false;
  }
}

/** Converts a validated protobuf response into JSON-safe, content-free data. */
export function terminalPipelineDiagnosticSnapshot(
  snapshot: WTerminalPipelineSnapshot,
  allowedSessionIds: ReadonlySet<string>,
): TerminalPipelineDiagnosticSnapshot {
  return {
    request_id: snapshot.requestId,
    dropped_targets: snapshot.droppedTargets,
    dropped_records: snapshot.droppedRecords,
    sessions: snapshot.sessions
      .filter((session) => allowedSessionIds.has(session.sessionId))
      .map((session) => ({
        session_id: session.sessionId,
        view_id: session.viewId,
        stages: session.stages.map((stage) => ({
          stage: stage.stage,
          reason: stage.reason,
          generation: stage.generation.toString(),
          stream_id: stage.streamId,
          sequence: stage.sequence.toString(),
          queue_frames: stage.queueFrames.toString(),
          queue_bytes: stage.queueBytes.toString(),
          native_buffered_bytes: stage.nativeBufferedBytes.toString(),
          oldest_age_ms: stage.oldestAgeMs.toString(),
          count: stage.count.toString(),
          histogram_buckets: stage.histogramBuckets.map((bucket) => bucket.toString()),
        })),
      })),
  };
}

export function normalizeTerminalPipelineDiagnosticTargets(
  targets: readonly TerminalPipelineDiagnosticTarget[],
): TerminalPipelineDiagnosticTarget[] {
  const uniqueTargets = new Map<string, TerminalPipelineDiagnosticTarget>();
  for (const target of targets) {
    if (
      !isBoundedText(target.sessionId, TERMINAL_PIPELINE_MAX_IDENTIFIER_BYTES)
      || !isBoundedText(target.viewId, TERMINAL_PIPELINE_MAX_IDENTIFIER_BYTES)
    ) continue;
    uniqueTargets.set(targetKey(target.sessionId, target.viewId), target);
  }
  return [...uniqueTargets.values()]
    .sort((left, right) => {
      if (left.sessionId !== right.sessionId) return left.sessionId < right.sessionId ? -1 : 1;
      return left.viewId === right.viewId ? 0 : left.viewId < right.viewId ? -1 : 1;
    })
    .slice(0, TERMINAL_PIPELINE_DIAG_MAX_TARGETS);
}

function terminalPipelineSnapshotMatchesRequest(
  snapshot: unknown,
  requestId: string,
  targets: readonly TerminalPipelineDiagnosticTarget[],
): snapshot is WTerminalPipelineSnapshot {
  if (!isTerminalPipelineSnapshotWireShape(snapshot) || snapshot.requestId !== requestId) return false;
  if (snapshot.droppedTargets !== 0 || snapshot.droppedRecords > targets.length) return false;
  const targetKeys = new Set(targets.map((target) => targetKey(target.sessionId, target.viewId)));
  const returnedTargetKeys = new Set<string>();
  for (const session of snapshot.sessions) {
    const key = targetKey(session.sessionId, session.viewId);
    if (!targetKeys.has(key)) return false;
    returnedTargetKeys.add(key);
  }
  return returnedTargetKeys.size + snapshot.droppedRecords === targets.length;

}

function isTerminalPipelineStageShape(value: unknown): boolean {
  if (value === null || typeof value !== "object") return false;
  const stage = value as {
    stage?: unknown;
    reason?: unknown;
    generation?: unknown;
    streamId?: unknown;
    sequence?: unknown;
    queueFrames?: unknown;
    queueBytes?: unknown;
    nativeBufferedBytes?: unknown;
    oldestAgeMs?: unknown;
    count?: unknown;
    histogramBuckets?: unknown;
  };
  return isWorkerPipelineStage(stage.stage)
    && isPipelineReason(stage.reason)
    && isUint64(stage.generation)
    && isBoundedText(stage.streamId, TERMINAL_PIPELINE_MAX_IDENTIFIER_BYTES)
    && isUint64(stage.sequence)
    && isUint64(stage.queueFrames)
    && isUint64(stage.queueBytes)
    && isUint64(stage.nativeBufferedBytes)
    && isUint64(stage.oldestAgeMs)
    && isUint64(stage.count)
    && Array.isArray(stage.histogramBuckets)
    && stage.histogramBuckets.length <= TERMINAL_PIPELINE_MAX_HISTOGRAM_BUCKETS
    && stage.histogramBuckets.every(isUint64);
}

function isWorkerPipelineStage(value: unknown): value is TerminalPipelineStage {
  return typeof value === "number"
    && Number.isInteger(value)
    && value >= TerminalPipelineStage.WORKER_PTY
    && value <= TerminalPipelineStage.WORKER_COORD_LINK;
}

function isPipelineReason(value: unknown): value is TerminalPipelineReason {
  return typeof value === "number"
    && Number.isInteger(value)
    && value >= TerminalPipelineReason.NONE
    && value <= TerminalPipelineReason.RAW_METADATA_PENDING;
}

function isUint32(value: unknown): value is number {
  return typeof value === "number" && Number.isInteger(value) && value >= 0 && value <= UINT32_MAX;
}

function isUint64(value: unknown): value is bigint {
  return typeof value === "bigint" && value >= 0n && value <= UINT64_MAX;
}

function isBoundedText(value: unknown, maximumBytes: number): value is string {
  return typeof value === "string" && Buffer.byteLength(value) <= maximumBytes;
}
function targetKey(sessionId: string, viewId: string): string {
  return JSON.stringify([sessionId, viewId]);
}
