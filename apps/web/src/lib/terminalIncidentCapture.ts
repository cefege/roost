// Production seam for opt-in terminal incident capture: lease control, frozen
// browser evidence and the capture calls the menu and the automatic detectors
// share. TerminalContextMenu's controller and terminalSnapshotFacade re-export
// these names; nothing else may drive a capture.
// Recorder state lives in terminalIncidentCaptureState.ts, evidence in
// terminalIncidentCaptureEvidence.ts, the wire call in
// terminalIncidentCaptureRpc.ts, result bookkeeping in
// terminalIncidentCaptureResult.ts, DOM reading in terminalIncidentDom.ts.

import { diag, signal } from "@roost/shared/diag";
import { isTerminalUuid } from "@roost/shared/viewport";
import {
  TERMINAL_CAPTURE_LIMITS,
  type TerminalCaptureActionName,
  type TerminalCaptureErrorCode,
  type TerminalCaptureReason,
  type TerminalCaptureResult,
} from "@roost/shared/terminal-capture";
import {
  canonicalTerminalFrame,
  hasTerminalSessionReplica,
} from "../store/terminal-stream-replica.ts";
import type { CellGridRenderer } from "./cellRenderer.ts";
import { isPageVisible } from "./pageVisible.ts";
import { rendererRegistryEntry } from "./terminalPreview.ts";
import { createIncidentObserver } from "./terminalIncidentCaptureObserver.ts";
import {
  downloadFrozenBrowserEvidence,
  freezeRecorderEvidence,
} from "./terminalIncidentCaptureEvidence.ts";
import {
  sendTerminalCaptureCommand,
  stoppedCaptureResult,
} from "./terminalIncidentCaptureRpc.ts";
import {
  applyCaptureResult,
  captureRefusal,
  reportCaptureExpiry,
} from "./terminalIncidentCaptureResult.ts";
import {
  anyTerminalRecorderArmed,
  armTerminalRecorder,
  armedTerminalRecorder,
  browserEvent,
  canArmAnotherRecording,
  disarmTerminalRecorder,
  emitTerminalCaptureUiState,
  ensureTerminalRecorder,
  expireTerminalRecorderIfLapsed,
  forgetTerminalRecorder,
  idleTerminalCaptureUiState,
  pushTerminalBrowserEvent,
  terminalCaptureUiStateOf,
  terminalRecorder,
  withConflictTally,
  type FrozenBrowserEvidence,
  type TerminalBrowserCaptureResult,
  type TerminalCaptureUiState,
  type TerminalConflictTally,
  type TerminalIncidentRecorder,
} from "./terminalIncidentCaptureState.ts";

export type { TerminalBrowserCaptureResult, TerminalCaptureUiState, TerminalConflictTally };

const frozenTokens = new Map<string, { sessionId: string; evidence: FrozenBrowserEvidence }>();

export function terminalCaptureUiState(sessionId: string): TerminalCaptureUiState {
  const recorder = terminalRecorder(sessionId);
  if (!recorder) return idleTerminalCaptureUiState(sessionId);
  if (expireTerminalRecorderIfLapsed(recorder, Date.now())) reportCaptureExpiry(recorder);
  return terminalCaptureUiStateOf(recorder);
}

export function subscribeTerminalCaptureUiState(
  sessionId: string,
  listener: (state: TerminalCaptureUiState) => void,
): () => void {
  const recorder = ensureTerminalRecorder(sessionId, crypto.randomUUID());
  recorder.listeners.add(listener);
  listener(terminalCaptureUiStateOf(recorder));
  return () => {
    recorder.listeners.delete(listener);
  };
}

/** Arm this document, the coordinator and the owning worker. A repeat START
 *  from the same page renews the same recording; it never stacks a second
 *  lease on an acknowledged one, and an expired lease is not resumed. */
export async function startTerminalCapture(
  sessionId: string,
): Promise<TerminalBrowserCaptureResult> {
  if (!isTerminalUuid(sessionId)) return captureRefusal(sessionId, "start", "invalid_argument");
  const existing = terminalRecorder(sessionId);
  if (existing && expireTerminalRecorderIfLapsed(existing, Date.now())) {
    reportCaptureExpiry(existing);
  }
  if (!canArmAnotherRecording(sessionId)) {
    return captureRefusal(sessionId, "start", "resource_exhausted");
  }
  const recorder = ensureTerminalRecorder(sessionId, crypto.randomUUID());
  if (!recorder.armed) {
    recorder.recordingId = crypto.randomUUID();
    recorder.phase = "arming";
    emitTerminalCaptureUiState(recorder);
  }
  const result = await sendLeaseCommand(recorder, "start");
  if (result.status === "recording") {
    armTerminalRecorder(recorder, result.expires_at_ms);
    bindRenderer(recorder, rendererRegistryEntry(sessionId)?.renderer ?? null);
    scheduleRenewal(recorder);
    diag("diag.terminal_capture_armed", {
      sid: sessionId,
      recording_id: recorder.recordingId,
      expires_at_ms: result.expires_at_ms,
    });
  } else if (!recorder.armed) {
    recorder.phase = "error";
  }
  return applyCaptureResult(recorder, result);
}

export async function captureTerminalIncident(
  sessionId: string,
  reason: TerminalCaptureReason,
): Promise<TerminalBrowserCaptureResult> {
  if (!isTerminalUuid(sessionId)) return captureRefusal(sessionId, "capture", "invalid_argument");
  const recorder = ensureTerminalRecorder(sessionId, crypto.randomUUID());
  if (expireTerminalRecorderIfLapsed(recorder, Date.now())) reportCaptureExpiry(recorder);
  const gate = manualCaptureGate(recorder);
  if (gate) return applyCaptureResult(recorder, captureRefusal(sessionId, "capture", gate));
  return sendFrozenEvidence(recorder, recorder.held ?? freezeEvidenceNow(recorder, reason), false);
}

export async function stopTerminalCapture(
  sessionId: string,
): Promise<TerminalBrowserCaptureResult> {
  if (!isTerminalUuid(sessionId)) return captureRefusal(sessionId, "stop", "invalid_argument");
  const recorder = terminalRecorder(sessionId);
  if (!recorder || recorder.phase === "idle") {
    const stopped = stoppedCaptureResult(sessionId, recorder?.recordingId ?? crypto.randomUUID());
    return withConflictTally(recorder ?? null, stopped);
  }
  const result = recorder.armed
    ? await sendLeaseCommand(recorder, "stop")
    : stoppedCaptureResult(sessionId, recorder.recordingId);
  disarmTerminalRecorder(recorder, "idle");
  recorder.held = null;
  dropTokensFor(sessionId);
  diag("diag.terminal_capture_stopped", { sid: sessionId, recording_id: recorder.recordingId });
  return applyCaptureResult(recorder, result);
}

/** Freeze the ephemeral DOM before a consent dialog can change it. The token
 *  is the only handle; a cancel discards it and sends nothing. */
export function freezeTerminalCaptureEvidence(
  sessionId: string,
  reason: TerminalCaptureReason,
): string {
  const recorder = ensureTerminalRecorder(sessionId, crypto.randomUUID());
  const evidence = recorder.held ?? freezeEvidenceNow(recorder, reason);
  const token = crypto.randomUUID();
  if (frozenTokens.size >= TERMINAL_CAPTURE_LIMITS.completedCaptureIds) {
    const oldest = frozenTokens.keys().next().value;
    if (oldest !== undefined) frozenTokens.delete(oldest);
  }
  frozenTokens.set(token, { sessionId, evidence });
  return token;
}

export function discardTerminalCaptureEvidence(token: string): void {
  frozenTokens.delete(token);
}

export async function captureTerminalIncidentFrozen(
  token: string,
): Promise<TerminalBrowserCaptureResult> {
  const entry = frozenTokens.get(token);
  if (!entry) return captureRefusal("", "capture", "invalid_argument");
  const recorder = ensureTerminalRecorder(entry.sessionId, entry.evidence.recordingId);
  const gate = manualCaptureGate(recorder);
  if (gate) return applyCaptureResult(recorder, captureRefusal(entry.sessionId, "capture", gate));
  const result = await sendFrozenEvidence(recorder, entry.evidence, false);
  if (result.status === "captured" && recorder.held === null) frozenTokens.delete(token);
  return result;
}

export function downloadLocalTerminalEvidence(token: string): void {
  const entry = frozenTokens.get(token);
  if (!entry) return;
  downloadFrozenBrowserEvidence(entry.evidence);
  diag("diag.terminal_capture_local_export", {
    sid: entry.sessionId,
    capture_id: entry.evidence.captureId,
  });
}

export function disposeTerminalIncidentRecorder(sessionId: string): void {
  dropTokensFor(sessionId);
  forgetTerminalRecorder(sessionId);
}

/** Scheduler entry point: binds a freshly mounted renderer to an armed
 *  recording and records the delivery boundary. */
export function noteTerminalRenderApply(
  sessionId: string,
  renderer: CellGridRenderer,
  mode: "full" | "delta" | "fallback_full",
): void {
  if (!anyTerminalRecorderArmed()) return;
  const recorder = armedTerminalRecorder(sessionId);
  if (!recorder) return;
  bindRenderer(recorder, renderer);
  const frame = canonicalTerminalFrame(sessionId);
  pushTerminalBrowserEvent(recorder, browserEvent("frame_received", frame, mode, null));
}

export function noteTerminalRenderApplied(
  sessionId: string,
  mode: "full" | "delta" | "fallback_full",
  applied: boolean,
): void {
  if (!anyTerminalRecorderArmed()) return;
  const recorder = armedTerminalRecorder(sessionId);
  if (!recorder) return;
  const kind = applied ? "render_applied" : "render_failed";
  const frame = canonicalTerminalFrame(sessionId);
  pushTerminalBrowserEvent(recorder, browserEvent(kind, frame, mode, null));
}

/** Pane teardown. A detached pane keeps its lease, because the replica
 *  outlives the renderer and a tab switch must not end a recording; a session
 *  whose replica is gone has nothing left to record. */
export function noteTerminalRendererDisposed(
  sessionId: string,
  renderer: CellGridRenderer,
): void {
  const recorder = terminalRecorder(sessionId);
  if (!recorder) return;
  if (recorder.renderer === renderer) {
    renderer.incidentObserver = null;
    recorder.renderer = null;
    recorder.observer = null;
    recorder.committed = null;
  }
  if (!hasTerminalSessionReplica(sessionId)) disposeTerminalIncidentRecorder(sessionId);
}

function bindRenderer(
  recorder: TerminalIncidentRecorder,
  renderer: CellGridRenderer | null,
): void {
  if (!renderer || recorder.renderer === renderer) return;
  if (recorder.renderer) recorder.renderer.incidentObserver = null;
  recorder.renderer = renderer;
  recorder.committed = null;
  const observer = createIncidentObserver(recorder, renderer, sendAutomaticCapture);
  recorder.observer = observer;
  renderer.incidentObserver = observer;
}

/** A visible debugging pane renews its own lease. A hidden tab, an unmounted
 *  pane or a lapsed lease does not: a reload must START again. */
function scheduleRenewal(recorder: TerminalIncidentRecorder): void {
  if (recorder.renewTimer !== null) return;
  recorder.renewTimer = setInterval(() => {
    if (expireTerminalRecorderIfLapsed(recorder, Date.now())) {
      reportCaptureExpiry(recorder);
      return;
    }
    if (!recorder.armed || !isPageVisible()) return;
    if (rendererRegistryEntry(recorder.sessionId) === undefined) return;
    void renewLease(recorder);
  }, TERMINAL_CAPTURE_LIMITS.renewIntervalMs);
}

/** START and STOP carry no evidence; a renewal is an idempotent START on the
 *  same recording identity. */
function sendLeaseCommand(
  recorder: TerminalIncidentRecorder,
  action: TerminalCaptureActionName,
): Promise<TerminalCaptureResult> {
  return sendTerminalCaptureCommand({
    sessionId: recorder.sessionId,
    recordingId: recorder.recordingId,
    action,
    reason: "manual",
    captureId: crypto.randomUUID(),
    browserEvidenceJson: "",
  });
}

async function renewLease(recorder: TerminalIncidentRecorder): Promise<void> {
  const result = await sendLeaseCommand(recorder, "start");
  if (result.status === "recording") recorder.expiresAtMs = result.expires_at_ms;
  else disarmTerminalRecorder(recorder, "error");
  applyCaptureResult(recorder, result);
}

/** Automatic path: the observer already froze the triggering DOM, so this only
 *  serializes and sends, off the render path. The live terminal never waits
 *  for a diagnostic RPC. */
function sendAutomaticCapture(recorder: TerminalIncidentRecorder): void {
  const reason = recorder.trigger?.reason ?? "history_identity";
  queueMicrotask(() => {
    if (!recorder.armed || recorder.captureInFlight) return;
    void sendFrozenEvidence(recorder, freezeEvidenceNow(recorder, reason), true);
  });
}

function freezeEvidenceNow(
  recorder: TerminalIncidentRecorder,
  reason: TerminalCaptureReason,
): FrozenBrowserEvidence {
  // An unarmed one-shot capture allocates no lease, so it carries its own
  // fresh recording identity and reports no prehistory.
  if (!recorder.armed && recorder.held === null) recorder.recordingId = crypto.randomUUID();
  return freezeRecorderEvidence(recorder, reason);
}

async function sendFrozenEvidence(
  recorder: TerminalIncidentRecorder,
  evidence: FrozenBrowserEvidence,
  automatic: boolean,
): Promise<TerminalBrowserCaptureResult> {
  recorder.captureInFlight = true;
  if (!automatic) recorder.lastManualAtMs = Date.now();
  try {
    const result = await sendTerminalCaptureCommand({
      sessionId: recorder.sessionId,
      recordingId: recorder.recordingId,
      action: "capture",
      reason: evidence.reason,
      captureId: evidence.captureId,
      browserEvidenceJson: evidence.wireJson,
    });
    // A trimmed payload stays held so the operator can still export the whole
    // frozen evidence locally; a complete upload releases it.
    recorder.held = result.status === "captured" && !evidence.partial ? null : evidence;
    if (result.error !== null || result.status === "error") {
      signal("terminal.capture_failed", {
        sid: recorder.sessionId,
        cooldownKey: evidence.captureId,
        recording_id: recorder.recordingId,
        error: result.error ?? result.status,
        automatic,
        held_evidence: recorder.held !== null,
      });
    }
    recorder.trigger = null;
    return applyCaptureResult(recorder, result);
  } finally {
    recorder.captureInFlight = false;
  }
}

function manualCaptureGate(recorder: TerminalIncidentRecorder): TerminalCaptureErrorCode | null {
  if (recorder.captureInFlight) return "capture_in_flight";
  if (Date.now() - recorder.lastManualAtMs < TERMINAL_CAPTURE_LIMITS.manualCooldownMs) {
    return "rate_limited";
  }
  return null;
}

function dropTokensFor(sessionId: string): void {
  for (const [token, entry] of frozenTokens) {
    if (entry.sessionId === sessionId) frozenTokens.delete(token);
  }
}
