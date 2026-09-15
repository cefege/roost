// Authenticated wire half of terminal incident capture: one DiagSnapshot call
// carrying a TerminalCaptureRequest, the response projection, and the fixed
// error vocabulary a failure maps to. No validator or parser text ever reaches
// a result, because those can quote the terminal content being validated.
// Called only by terminalIncidentCapture.ts; the request shape comes from the
// generated coordinator proto and @roost/shared/terminal-capture.

import { create } from "@bufbuild/protobuf";
import { Code, ConnectError } from "@connectrpc/connect";
import { TerminalCaptureRequestSchema } from "@roost/shared/proto/coordinator_pb";
import {
  terminalCaptureActionValue,
  type TerminalCaptureActionName,
  type TerminalCaptureErrorCode,
  type TerminalCaptureReason,
  type TerminalCaptureResult,
} from "@roost/shared/terminal-capture";
import { coordClient } from "../connect.ts";
import { terminalBrowserStreamSnapshot } from "./terminalDiagSnapshot.ts";

export interface CaptureCommandInput {
  readonly sessionId: string;
  readonly recordingId: string;
  readonly action: TerminalCaptureActionName;
  readonly reason: TerminalCaptureReason;
  readonly captureId: string;
  readonly browserEvidenceJson: string;
}

export async function sendTerminalCaptureCommand(
  input: CaptureCommandInput,
): Promise<TerminalCaptureResult> {
  const terminalCapture = create(TerminalCaptureRequestSchema, {
    action: terminalCaptureActionValue(input.action),
    sessionId: input.sessionId,
    captureId: input.captureId,
    recordingId: input.recordingId,
    reason: input.reason,
    browserEvidenceJson: input.browserEvidenceJson,
  });
  try {
    // Same request shape the ordinary content-free snapshot uses, plus the
    // capture command; the session filter must be exactly this capture's
    // session, because the bridge refuses the legacy scalar filter here.
    const response = await coordClient.diagSnapshot({
      sessionFilterIds: [input.sessionId],
      spaStateJson: JSON.stringify(terminalBrowserStreamSnapshot(input.sessionId)),
      terminalCapture,
    });
    return readCaptureResult(response?.snapshotJson, input);
  } catch (error) {
    return localCaptureResult(input, captureErrorCode(error));
  }
}

/** `snapshot_json.terminal_capture` is exactly a TerminalCaptureResult. A
 *  response that carries no projection is an internal failure, not a success
 *  with empty fields. */
function readCaptureResult(
  snapshotJson: string | undefined,
  input: CaptureCommandInput,
): TerminalCaptureResult {
  let projected: unknown;
  try {
    projected = typeof snapshotJson === "string" && snapshotJson.length > 0
      ? (JSON.parse(snapshotJson) as Record<string, unknown>).terminal_capture
      : undefined;
  } catch {
    projected = undefined;
  }
  if (projected === null || typeof projected !== "object") {
    return localCaptureResult(input, "internal");
  }
  return projected as TerminalCaptureResult;
}

export function localCaptureResult(
  input: CaptureCommandInput,
  error: TerminalCaptureErrorCode,
): TerminalCaptureResult {
  return {
    capture_id: input.captureId,
    recording_id: input.recordingId,
    session_id: input.sessionId,
    action: input.action,
    status: "error",
    expires_at_ms: null,
    worker_fp: null,
    path: null,
    byte_length: null,
    error,
    recent_worker_capture: null,
  };
}

/** A session with no live recording is already stopped; saying so locally
 *  keeps STOP idempotent without inventing a lease to release. */
export function stoppedCaptureResult(
  sessionId: string,
  recordingId: string,
): TerminalCaptureResult {
  return {
    capture_id: crypto.randomUUID(),
    recording_id: recordingId,
    session_id: sessionId,
    action: "stop",
    status: "stopped",
    expires_at_ms: null,
    worker_fp: null,
    path: null,
    byte_length: null,
    error: null,
    recent_worker_capture: null,
  };
}

/** The bridge reports caller faults as `"<code>: <field>"`, so the exact code
 *  survives the status mapping (evidence_too_large and invalid_argument share
 *  one gRPC code). The table is keyed by the shared union, so a new code in
 *  @roost/shared fails this file's typecheck instead of silently degrading. */
function captureErrorCode(error: unknown): TerminalCaptureErrorCode {
  if (!(error instanceof ConnectError)) return "internal";
  const declared = error.rawMessage.split(": ", 1)[0] ?? "";
  if (Object.hasOwn(CAPTURE_ERROR_CODES, declared)) return declared as TerminalCaptureErrorCode;
  switch (error.code) {
    case Code.InvalidArgument: return "invalid_argument";
    case Code.Unauthenticated:
    case Code.PermissionDenied: return "permission_denied";
    case Code.NotFound: return "session_unknown";
    case Code.AlreadyExists: return "lease_conflict";
    case Code.FailedPrecondition: return "lease_absent";
    case Code.Aborted: return "capture_in_flight";
    case Code.ResourceExhausted: return "resource_exhausted";
    case Code.DeadlineExceeded: return "worker_timeout";
    case Code.Unavailable: return "worker_offline";
    default: return "internal";
  }
}

const CAPTURE_ERROR_CODES: Record<TerminalCaptureErrorCode, true> = {
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
