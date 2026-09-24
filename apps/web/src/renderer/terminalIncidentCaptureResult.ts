// Result bookkeeping for terminal incident capture: turns a wire or locally
// authored result into the result the menu sees, stamps it with the recorder's
// accumulated conflict counters, records it for the UI state and reports a
// lapsed lease.
// Called by terminalIncidentCapture.ts; result shapes come from
// terminalIncidentCaptureRpc.ts, recorder state from
// terminalIncidentCaptureState.ts.

import { signal } from "@roost/observability/diag";
import type {
  TerminalCaptureActionName,
  TerminalCaptureErrorCode,
  TerminalCaptureResult,
} from "@roost/protocol/terminal-capture";
import { localCaptureResult } from "./terminalIncidentCaptureRpc.ts";
import {
  emitTerminalCaptureUiState,
  terminalRecorder,
  withConflictTally,
  type TerminalBrowserCaptureResult,
  type TerminalIncidentRecorder,
} from "./terminalIncidentCaptureState.ts";

/** Every result the browser hands back carries the conflict counters, so a
 *  persistent invariant violation is reported once as a Tier-1 signal and
 *  thereafter only as a number. */
export function applyCaptureResult(
  recorder: TerminalIncidentRecorder,
  result: TerminalCaptureResult,
): TerminalBrowserCaptureResult {
  const carried = withConflictTally(recorder, result);
  recorder.lastResult = carried;
  recorder.lastError = result.error;
  if (result.error !== null && !recorder.armed && recorder.phase !== "expired") {
    recorder.phase = "error";
  }
  emitTerminalCaptureUiState(recorder);
  return carried;
}

/** A command that never reached the coordinator still answers in the shared
 *  result shape, with a fixed error code and no validator text. */
export function captureRefusal(
  sessionId: string,
  action: TerminalCaptureActionName,
  error: TerminalCaptureErrorCode,
): TerminalBrowserCaptureResult {
  const recorder = terminalRecorder(sessionId) ?? null;
  return withConflictTally(recorder, localCaptureResult(
    {
      sessionId,
      recordingId: recorder?.recordingId ?? "",
      action,
      reason: "manual",
      captureId: crypto.randomUUID(),
      browserEvidenceJson: "",
    },
    error,
  ));
}

export function reportCaptureExpiry(recorder: TerminalIncidentRecorder): void {
  signal("terminal.capture_expired", {
    sid: recorder.sessionId,
    cooldownKey: recorder.recordingId,
    recording_id: recorder.recordingId,
  });
  emitTerminalCaptureUiState(recorder);
}
