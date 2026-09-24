// One dedicated, correlated terminal-capture call to the session's own worker.
// Called only by terminal-capture.ts; reuses the pending-RPC correlation table
// and the browser-command downstream frame, with the capture deadline instead
// of the ordinary 2s diagnostic one.
// The worker's reply crosses a trust boundary, so this module rebuilds the
// acknowledgement from the fields it recognizes and rejects everything else.

import { create } from "@bufbuild/protobuf";
import { Code, ConnectError } from "@connectrpc/connect";
import {
  CoordWorkerDownSchema,
  DBrowserCommandSchema,
} from "@roost/protocol/proto/worker_transport_pb";
import {
  TERMINAL_CAPTURE_LIMITS,
  type TerminalCaptureCommand,
  type TerminalCaptureErrorCode,
  type TerminalCaptureFileRef,
  type TerminalCaptureStatus,
} from "@roost/protocol/terminal-capture";
import { createPendingRpc, rejectPendingRpcUnavailable } from "../../router/pending-rpcs.ts";
import { connectWorkers } from "../../workers/worker-registry.ts";

export interface TerminalCaptureWorkerAck {
  readonly status: TerminalCaptureStatus;
  readonly path: string | null;
  readonly byteLength: number | null;
  readonly error: TerminalCaptureErrorCode | null;
  readonly expiresAtMs: number | null;
  readonly recentWorkerCapture: TerminalCaptureFileRef | null;
}

export type TerminalCaptureWorkerOutcome =
  | { readonly ok: true; readonly ack: TerminalCaptureWorkerAck }
  | { readonly ok: false; readonly code: "worker_offline" | "worker_timeout" | "worker_failed" };

/** A worker-chosen absolute path under its own 0700 log directory. The name is
 *  a fixed-length capture UUID, so this only has to stop an unbounded string
 *  from reaching the browser and the response projection. */
const WORKER_PATH_MAX_CHARS = 1_024;
const WORKER_CAPTURE_ID_MAX_CHARS = 64;

// Exhaustive by construction: a literal added to the shared union fails this
// file's typecheck instead of silently passing an unknown status through.
const WORKER_STATUS: Record<TerminalCaptureStatus, true> = {
  recording: true,
  captured: true,
  stopped: true,
  partial: true,
  error: true,
};

const WORKER_ERROR: Record<TerminalCaptureErrorCode, true> = {
  invalid_argument: true,
  permission_denied: true,
  session_unknown: true,
  worker_offline: true,
  worker_timeout: true,
  worker_failed: true,
  lease_conflict: true,
  lease_expired: true,
  lease_absent: true,
  resource_exhausted: true,
  evidence_too_large: true,
  evidence_malformed: true,
  capture_in_flight: true,
  rate_limited: true,
  capture_expired: true,
  storage_failed: true,
  internal: true,
};

export async function requestTerminalCapture(
  workerFp: string,
  command: TerminalCaptureCommand,
  coordinatorEvidenceJson: string,
  /** The contract's capture deadline. Only the focused failure-mapping test
   *  passes a shorter one, so a deadline proof costs no wall clock. */
  deadlineMs: number = TERMINAL_CAPTURE_LIMITS.captureDeadlineMs,
): Promise<TerminalCaptureWorkerOutcome> {
  const worker = connectWorkers.get(workerFp);
  if (!worker || !worker.ready || worker.revoked) return { ok: false, code: "worker_offline" };

  const pending = createPendingRpc<unknown>(deadlineMs, workerFp);
  try {
    const sent = worker.send(create(CoordWorkerDownSchema, {
      frame: {
        case: "browserCommand",
        value: create(DBrowserCommandSchema, {
          browserId: "coordinator-diag",
          viewerId: "coordinator-diag",
          requestId: pending.request_id,
          frameJson: JSON.stringify({
            kind: "diag-terminal-capture",
            request_id: pending.request_id,
            session_id: command.session_id,
            recording_id: command.recording_id,
            capture_id: command.capture_id,
            action: command.action,
            reason: command.reason,
            browser_evidence_json: command.browser_evidence_json,
            coordinator_evidence_json: coordinatorEvidenceJson,
          }),
        }),
      },
    }));
    if (sent === 0) {
      rejectPendingRpcUnavailable(
        pending.request_id,
        "worker transport dropped terminal capture request",
        workerFp,
      );
      await pending.promise.catch(() => undefined);
      return { ok: false, code: "worker_offline" };
    }
  } catch {
    rejectPendingRpcUnavailable(
      pending.request_id,
      "worker transport failed terminal capture request",
      workerFp,
    );
    await pending.promise.catch(() => undefined);
    return { ok: false, code: "worker_failed" };
  }

  try {
    const ack = narrowWorkerAck(await pending.promise);
    return ack === null ? { ok: false, code: "worker_failed" } : { ok: true, ack };
  } catch (error) {
    if (error instanceof ConnectError) {
      if (error.code === Code.DeadlineExceeded) return { ok: false, code: "worker_timeout" };
      if (error.code === Code.Unavailable) return { ok: false, code: "worker_offline" };
    }
    return { ok: false, code: "worker_failed" };
  }
}

/** Rebuild the acknowledgement from recognized fields only. One unexpected
 *  shape is a worker failure, never a partially trusted result: the path and
 *  byte length end up in an operator-visible download action. */
function narrowWorkerAck(value: unknown): TerminalCaptureWorkerAck | null {
  if (value === null || typeof value !== "object" || Array.isArray(value)) return null;
  const data = value as Record<string, unknown>;
  const status = data.status;
  if (typeof status !== "string" || WORKER_STATUS[status as TerminalCaptureStatus] !== true) {
    return null;
  }
  const path = narrowWorkerPath(data.path);
  if (path === undefined) return null;
  const byteLength = narrowWorkerCount(data.byte_length);
  if (byteLength === undefined) return null;
  const expiresAtMs = narrowWorkerCount(data.expires_at_ms);
  if (expiresAtMs === undefined) return null;
  const error = data.error;
  let narrowedError: TerminalCaptureErrorCode | null = null;
  if (error !== null && error !== undefined && error !== "") {
    if (typeof error !== "string" || WORKER_ERROR[error as TerminalCaptureErrorCode] !== true) {
      return null;
    }
    narrowedError = error as TerminalCaptureErrorCode;
  }
  const recent = narrowWorkerFileRef(data.recent_worker_capture);
  if (recent === undefined) return null;
  return {
    status: status as TerminalCaptureStatus,
    path,
    byteLength,
    error: narrowedError,
    expiresAtMs,
    recentWorkerCapture: recent,
  };
}

/** `undefined` = reject the whole acknowledgement; `null` = field absent. */
function narrowWorkerPath(value: unknown): string | null | undefined {
  if (value === null || value === undefined || value === "") return null;
  if (typeof value !== "string" || value.length > WORKER_PATH_MAX_CHARS) return undefined;
  return value;
}

function narrowWorkerCount(value: unknown): number | null | undefined {
  if (value === null || value === undefined) return null;
  if (typeof value !== "number" || !Number.isSafeInteger(value) || value < 0) return undefined;
  return value;
}

function narrowWorkerFileRef(value: unknown): TerminalCaptureFileRef | null | undefined {
  if (value === null || value === undefined) return null;
  if (typeof value !== "object" || Array.isArray(value)) return undefined;
  const ref = value as Record<string, unknown>;
  const captureId = ref.capture_id;
  const path = narrowWorkerPath(ref.path);
  const byteLength = narrowWorkerCount(ref.byte_length);
  const status = ref.status;
  if (
    typeof captureId !== "string"
    || captureId.length === 0
    || captureId.length > WORKER_CAPTURE_ID_MAX_CHARS
    || path === undefined
    || path === null
    || byteLength === undefined
    || byteLength === null
    || typeof status !== "string"
    || WORKER_STATUS[status as TerminalCaptureStatus] !== true
  ) return undefined;
  return {
    capture_id: captureId,
    path,
    byte_length: byteLength,
    status: status as TerminalCaptureStatus,
  };
}
