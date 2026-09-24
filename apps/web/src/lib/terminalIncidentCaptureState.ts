// Per-session recorder state for opt-in terminal incident capture: the lease a
// START acknowledged, the bounded event ring, the committed painted model, the
// frozen evidence a failed upload still holds, and the latches that bound both
// automatic captures and the one signal line each identity may write. The
// lease/observer/result modules of this family are its only mutators;
// terminalIncidentCaptureEvidence.ts reads a recorder to build a payload.
// No DOM, no network, no timers are created here.

import {
  TERMINAL_CAPTURE_LIMITS,
  type TerminalBrowserApplyMode,
  type TerminalBrowserEvent,
  type TerminalBrowserPaintedState,
  type TerminalCaptureErrorCode,
  type TerminalCaptureReason,
  type TerminalCaptureResult,
  type TerminalCaptureTrigger,
} from "@roost/protocol/terminal-capture";
import type { CellGridFrame } from "@roost/protocol/cell";
import type { CellGridRenderer } from "./cellRenderer.ts";
import type { RendererIncidentObserver } from "./cellRendererPresentation.ts";
import { streamIdentityOfFrame, type CommittedPaintedModel } from "./terminalIncidentDom.ts";

export type TerminalCapturePhase = "idle" | "arming" | "recording" | "expired" | "error";

export interface TerminalCaptureUiState {
  readonly sessionId: string;
  readonly phase: TerminalCapturePhase;
  readonly recordingId: string | null;
  readonly expiresAtMs: number | null;
  readonly lastResult: TerminalCaptureResult | null;
  readonly lastError: TerminalCaptureErrorCode | null;
  readonly heldEvidence: boolean;
}

/** Bounded accounting for painted-invariant violations. A persistent violation
 *  is re-observed at every history boundary, so it emits ONE Tier-1 line per
 *  `(lease, stream, epoch, reason)` identity and accumulates here: `identities`
 *  is exactly how many `terminal.history_conflict` lines this lease wrote. */
export interface TerminalConflictTally {
  readonly occurrences: number;
  readonly identities: number;
  readonly captured: number;
  /** Identities the bounded identity map could no longer distinguish. */
  readonly dropped_identities: number;
}

/** What the browser adds to the wire result: the counters that replace the
 *  repeated signal lines, so a capture and a STOP report how often the
 *  invariant actually fired. */
export interface TerminalBrowserCaptureResult extends TerminalCaptureResult {
  readonly conflicts: TerminalConflictTally;
}

/** One frozen browser payload. `payload` is the untrimmed evidence retained for
 *  a local download; `wireJson` is the copy already fitted to the request
 *  budget, which may be metadata-only. */
export interface FrozenBrowserEvidence {
  readonly captureId: string;
  readonly recordingId: string;
  readonly reason: TerminalCaptureReason;
  readonly frozenAtMs: number;
  readonly payload: unknown;
  readonly wireJson: string;
  readonly partial: boolean;
}

export interface TerminalIncidentRecorder {
  readonly sessionId: string;
  phase: TerminalCapturePhase;
  recordingId: string;
  /** True only while a START has been acknowledged and the lease is live. */
  armed: boolean;
  expiresAtMs: number | null;
  lastResult: TerminalCaptureResult | null;
  lastError: TerminalCaptureErrorCode | null;
  renewTimer: ReturnType<typeof setInterval> | null;
  renderer: CellGridRenderer | null;
  observer: RendererIncidentObserver | null;
  events: TerminalBrowserEvent[];
  droppedEvents: number;
  droppedRows: number;
  /** Stream the last committed painting belonged to. */
  streamId: string | null;
  committed: CommittedPaintedModel | null;
  preState: TerminalBrowserPaintedState | null;
  triggerState: TerminalBrowserPaintedState | null;
  preRepairState: TerminalBrowserPaintedState | null;
  postRepairState: TerminalBrowserPaintedState | null;
  trigger: TerminalCaptureTrigger | null;
  held: FrozenBrowserEvidence | null;
  /** `(stream, epoch, reason)` identities an automatic capture already used. */
  latches: Set<string>;
  occurrences: Map<string, number>;
  /** Survives a disarm so the STOP that freed the lease still reports it. */
  conflicts: TerminalConflictTally;
  lastAutomaticAtMs: number;
  lastManualAtMs: number;
  lastSampleAtMs: number;
  captureInFlight: boolean;
  /** Reset at every apply so one DOM scan serves one apply cycle. */
  cyclePreState: boolean;
  cycleHistoryChecked: boolean;
  cycleDestructive: boolean;
  listeners: Set<(state: TerminalCaptureUiState) => void>;
}

const recorders = new Map<string, TerminalIncidentRecorder>();
const NO_CONFLICTS: TerminalConflictTally = {
  occurrences: 0,
  identities: 0,
  captured: 0,
  dropped_identities: 0,
};

/** Read before every observer hook and wire hook, so an unarmed document does
 *  no map lookup, no DOM read and no allocation. */
let armedRecordings = 0;

export function anyTerminalRecorderArmed(): boolean {
  return armedRecordings > 0;
}

export function terminalRecorder(sessionId: string): TerminalIncidentRecorder | undefined {
  return recorders.get(sessionId);
}

export function armedTerminalRecorder(sessionId: string): TerminalIncidentRecorder | null {
  if (armedRecordings === 0) return null;
  const recorder = recorders.get(sessionId);
  return recorder?.armed === true ? recorder : null;
}

export function ensureTerminalRecorder(
  sessionId: string,
  recordingId: string,
): TerminalIncidentRecorder {
  const existing = recorders.get(sessionId);
  if (existing) return existing;
  const recorder: TerminalIncidentRecorder = {
    sessionId,
    phase: "idle",
    recordingId,
    armed: false,
    expiresAtMs: null,
    lastResult: null,
    lastError: null,
    renewTimer: null,
    renderer: null,
    observer: null,
    events: [],
    droppedEvents: 0,
    droppedRows: 0,
    streamId: null,
    committed: null,
    preState: null,
    triggerState: null,
    preRepairState: null,
    postRepairState: null,
    trigger: null,
    held: null,
    latches: new Set(),
    occurrences: new Map(),
    conflicts: NO_CONFLICTS,
    lastAutomaticAtMs: 0,
    lastManualAtMs: 0,
    lastSampleAtMs: 0,
    captureInFlight: false,
    cyclePreState: false,
    cycleHistoryChecked: false,
    cycleDestructive: false,
    listeners: new Set(),
  };
  recorders.set(sessionId, recorder);
  return recorder;
}

/** False when this document already holds its maximum acknowledged leases; the
 *  caller reports resource_exhausted rather than evicting another pane. */
export function canArmAnotherRecording(sessionId: string): boolean {
  if (recorders.get(sessionId)?.armed === true) return true;
  return armedRecordings < TERMINAL_CAPTURE_LIMITS.maxRecordingsPerDocument;
}

export function armTerminalRecorder(
  recorder: TerminalIncidentRecorder,
  expiresAtMs: number | null,
): void {
  // A fresh lease is fresh accounting; a renewal keeps what it has counted.
  if (!recorder.armed) {
    armedRecordings++;
    recorder.conflicts = NO_CONFLICTS;
  }
  recorder.armed = true;
  recorder.phase = "recording";
  recorder.expiresAtMs = expiresAtMs;
}

/** Free everything the recording owns except the frozen payload a failed
 *  upload still holds, which survives until retry, STOP or tab close. */
export function disarmTerminalRecorder(
  recorder: TerminalIncidentRecorder,
  phase: TerminalCapturePhase,
): void {
  if (recorder.armed) armedRecordings--;
  recorder.armed = false;
  recorder.phase = phase;
  recorder.expiresAtMs = null;
  if (recorder.renewTimer !== null) {
    clearInterval(recorder.renewTimer);
    recorder.renewTimer = null;
  }
  if (recorder.renderer) recorder.renderer.incidentObserver = null;
  recorder.renderer = null;
  recorder.observer = null;
  recorder.events = [];
  recorder.droppedEvents = 0;
  recorder.droppedRows = 0;
  recorder.streamId = null;
  recorder.committed = null;
  recorder.preState = null;
  recorder.triggerState = null;
  recorder.preRepairState = null;
  recorder.postRepairState = null;
  recorder.trigger = null;
  recorder.latches.clear();
  recorder.occurrences.clear();
  recorder.cyclePreState = false;
  recorder.cycleHistoryChecked = false;
  recorder.cycleDestructive = false;
}

export function forgetTerminalRecorder(sessionId: string): void {
  const recorder = recorders.get(sessionId);
  if (!recorder) return;
  disarmTerminalRecorder(recorder, "idle");
  recorder.held = null;
  recorder.listeners.clear();
  recorders.delete(sessionId);
}

/** A lease that passed its server-time expiry disarms itself. A page reload
 *  must START again; nothing here silently renews. */
export function expireTerminalRecorderIfLapsed(
  recorder: TerminalIncidentRecorder,
  nowMs: number,
): boolean {
  if (!recorder.armed || recorder.expiresAtMs === null || nowMs < recorder.expiresAtMs) {
    return false;
  }
  disarmTerminalRecorder(recorder, "expired");
  return true;
}

export function terminalCaptureUiStateOf(
  recorder: TerminalIncidentRecorder,
): TerminalCaptureUiState {
  return {
    sessionId: recorder.sessionId,
    phase: recorder.phase,
    recordingId: recorder.phase === "idle" ? null : recorder.recordingId,
    expiresAtMs: recorder.expiresAtMs,
    lastResult: recorder.lastResult,
    lastError: recorder.lastError,
    heldEvidence: recorder.held !== null,
  };
}

export function idleTerminalCaptureUiState(sessionId: string): TerminalCaptureUiState {
  return {
    sessionId,
    phase: "idle",
    recordingId: null,
    expiresAtMs: null,
    lastResult: null,
    lastError: null,
    heldEvidence: false,
  };
}

export function emitTerminalCaptureUiState(recorder: TerminalIncidentRecorder): void {
  if (recorder.listeners.size === 0) return;
  const state = terminalCaptureUiStateOf(recorder);
  for (const listener of recorder.listeners) listener(state);
}

export function pushTerminalBrowserEvent(
  recorder: TerminalIncidentRecorder,
  event: TerminalBrowserEvent,
): void {
  recorder.events.push(event);
  while (recorder.events.length > TERMINAL_CAPTURE_LIMITS.layerEntries) {
    recorder.events.shift();
    recorder.droppedEvents++;
  }
}

export function browserEvent(
  kind: TerminalBrowserEvent["kind"],
  frame: CellGridFrame | null,
  applyMode: TerminalBrowserApplyMode | null,
  detail: string | null,
): TerminalBrowserEvent {
  return {
    at_ms: Date.now(),
    kind,
    stream: streamIdentityOfFrame(frame),
    apply_mode: applyMode,
    detail,
  };
}

/** Wire-side admission and repair boundaries. The first line is the unarmed
 *  gate: an ordinary terminal pays one integer comparison per frame. */
export function noteTerminalReplicaTransition(
  sessionId: string,
  kind: TerminalBrowserEvent["kind"],
  frame: CellGridFrame | null,
  detail: string | null,
): void {
  if (armedRecordings === 0) return;
  const recorder = armedTerminalRecorder(sessionId);
  if (!recorder) return;
  pushTerminalBrowserEvent(recorder, browserEvent(kind, frame, null, detail));
}

export interface AutomaticCaptureAdmission {
  readonly allowed: boolean;
  readonly latch: string;
  readonly occurrences: number;
  /** True exactly once per identity, and the only thing permitted to write a
   *  Tier-1 line for it. */
  readonly firstForIdentity: boolean;
}

/** One automatic capture per `(recording lease, stream, grid epoch, reason)`,
 *  and never two within the session-wide cooldown — a new epoch does not buy a
 *  fresh budget. The latch is taken BEFORE any RPC.
 *  A violation that persists is re-observed at every painted boundary, so that
 *  same identity also decides whether a signal line may be written: the generic
 *  per-kind cooldown would otherwise let one stuck duplicate report itself
 *  every 10 seconds for as long as the pane stays open. Repeats accumulate in
 *  the tally the capture and STOP results carry. */
export function admitAutomaticCapture(
  recorder: TerminalIncidentRecorder,
  streamId: string | null,
  gridEpoch: string | null,
  reason: TerminalCaptureReason,
  nowMs: number,
): AutomaticCaptureAdmission {
  const latch = `${recorder.recordingId}|${streamId ?? ""}|${gridEpoch ?? ""}|${reason}`;
  const previous = recorder.occurrences.get(latch);
  const tracked = previous !== undefined
    || recorder.occurrences.size < TERMINAL_CAPTURE_LIMITS.layerEntries;
  const occurrences = (previous ?? 0) + 1;
  if (tracked) recorder.occurrences.set(latch, occurrences);
  const firstForIdentity = tracked && previous === undefined;
  const allowed = !recorder.latches.has(latch)
    && !recorder.captureInFlight
    && nowMs - recorder.lastAutomaticAtMs >= TERMINAL_CAPTURE_LIMITS.automaticCooldownMs;
  if (allowed) {
    recorder.latches.add(latch);
    recorder.lastAutomaticAtMs = nowMs;
  }
  recorder.conflicts = {
    occurrences: recorder.conflicts.occurrences + 1,
    identities: recorder.conflicts.identities + (firstForIdentity ? 1 : 0),
    captured: recorder.conflicts.captured + (allowed ? 1 : 0),
    dropped_identities: recorder.conflicts.dropped_identities + (tracked ? 0 : 1),
  };
  return { allowed, latch, occurrences, firstForIdentity };
}

/** Carry the conflict counters out with every result the browser returns. */
export function withConflictTally(
  recorder: TerminalIncidentRecorder | null,
  result: TerminalCaptureResult,
): TerminalBrowserCaptureResult {
  return { ...result, conflicts: recorder?.conflicts ?? NO_CONFLICTS };
}

/** True when an expensive DOM sample is due. Cheap per-frame states reuse the
 *  recorder's sampling cadence instead of scanning on every sparse update. */
export function sampleDue(recorder: TerminalIncidentRecorder, nowMs: number): boolean {
  if (nowMs - recorder.lastSampleAtMs < TERMINAL_CAPTURE_LIMITS.coreSampleIntervalMs) {
    return false;
  }
  recorder.lastSampleAtMs = nowMs;
  return true;
}

export function _resetTerminalIncidentRecorders(): void {
  for (const sessionId of [...recorders.keys()]) forgetTerminalRecorder(sessionId);
  armedRecordings = 0;
}
