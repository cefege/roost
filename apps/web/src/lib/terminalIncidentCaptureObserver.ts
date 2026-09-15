// Renderer observer for one armed terminal incident recorder: retains the most
// recent bounded pre-state, records apply/repair boundaries, and evaluates the
// strict painted-identity invariants before a destructive repair can erase
// their evidence. It never repairs, scrolls, selects or sends PTY input.
// Installed by terminalIncidentCapture.ts, which supplies the trigger callback;
// DOM reading and comparison live in terminalIncidentDom.ts.

import { signal } from "@roost/shared/diag";
import {
  type TerminalBrowserApplyMode,
  type TerminalBrowserPaintedState,
  type TerminalBrowserPhase,
  type TerminalCaptureReason,
  type TerminalCaptureStreamIdentity,
} from "@roost/shared/terminal-capture";
import type { CellGridRenderer } from "./cellRenderer.ts";
import type {
  RendererIncidentObserver,
  RendererIncidentPhase,
  RendererProjection,
} from "./cellRendererPresentation.ts";
import { isPageVisible } from "./pageVisible.ts";
import { rendererRegistryEntry } from "./terminalPreview.ts";
import { canonicalTerminalFrame } from "../store/terminal-stream-replica.ts";
import {
  admitAutomaticCapture,
  browserEvent,
  expireTerminalRecorderIfLapsed,
  pushTerminalBrowserEvent,
  sampleDue,
  type TerminalIncidentRecorder,
} from "./terminalIncidentCaptureState.ts";
import {
  committedPaintedModel,
  findPaintedConflict,
  readPaintedState,
  type CommittedPaintedModel,
  type PaintedConflict,
} from "./terminalIncidentDom.ts";

export type IncidentTriggerHandler = (recorder: TerminalIncidentRecorder) => void;

const BROWSER_PHASE: Record<RendererIncidentPhase, TerminalBrowserPhase> = {
  pre_apply: "pre_apply",
  pre_destructive: "pre_destructive",
  pre_history_insert: "pre_history_insert",
  post_reconcile: "post_reconcile",
};

export function createIncidentObserver(
  recorder: TerminalIncidentRecorder,
  renderer: CellGridRenderer,
  onTrigger: IncidentTriggerHandler,
): RendererIncidentObserver {
  return {
    get armed(): boolean {
      return recorder.armed;
    },
    observe(phase: RendererIncidentPhase, mode: "full" | "delta" | null): void {
      observeRendererPhase(recorder, renderer, phase, mode, onTrigger);
    },
  };
}

/** One bounded painted snapshot, read from the live DOM. */
export function capturePaintedState(
  recorder: TerminalIncidentRecorder,
  projection: RendererProjection,
  phase: TerminalBrowserPhase,
  applyMode: TerminalBrowserApplyMode | null,
): TerminalBrowserPaintedState {
  const owner = rendererRegistryEntry(recorder.sessionId);
  let active = false;
  try {
    active = owner?.ownerSource?.().slot.surface_active === true;
  } catch {
    // A diagnostic read must never perturb terminal ownership or rendering.
  }
  const state = readPaintedState({
    projection,
    phase,
    applyMode,
    canonical: canonicalTerminalFrame(recorder.sessionId),
    committed: committedStreamIdentity(recorder),
    active,
    visible: isPageVisible(),
  });
  for (const omission of state.omissions) recorder.droppedRows += omission.dropped_count;
  return state;
}

export function committedStreamIdentity(
  recorder: TerminalIncidentRecorder,
): TerminalCaptureStreamIdentity | null {
  const committed = recorder.committed;
  if (!committed) return null;
  return {
    stream_id: recorder.streamId ?? "",
    grid_epoch: committed.gridEpoch,
    seq: String(committed.seq),
    base_seq: null,
    cols: committed.cols,
    rows: committed.rows,
  };
}

function observeRendererPhase(
  recorder: TerminalIncidentRecorder,
  renderer: CellGridRenderer,
  phase: RendererIncidentPhase,
  mode: "full" | "delta" | null,
  onTrigger: IncidentTriggerHandler,
): void {
  const now = Date.now();
  if (expireTerminalRecorderIfLapsed(recorder, now)) return;
  const projection = renderer.rendererProjection();
  if (phase === "pre_apply") {
    recorder.cyclePreState = false;
    recorder.cycleHistoryChecked = false;
    recorder.cycleDestructive = false;
    pushTerminalBrowserEvent(recorder, browserEvent("render_scheduled", projection.canonical, mode, null));
    if (mode === "full" || sampleDue(recorder, now)) {
      retainPreState(recorder, projection, "pre_apply", mode);
    }
    return;
  }
  if (phase === "pre_destructive") {
    retainPreState(recorder, projection, "pre_destructive", mode);
    pushTerminalBrowserEvent(recorder, browserEvent("destructive_full", projection.canonical, mode, null));
    evaluateInvariants(recorder, projection, recorder.committed, true, "pre_destructive", onTrigger);
    recorder.cycleDestructive = true;
    return;
  }
  if (phase === "pre_history_insert") {
    // Inside a destructive repair the history container is mid-rebuild, so it
    // is not evidence of anything; the pre-destructive read already ran.
    if (recorder.cycleDestructive) return;
    if (!recorder.cyclePreState) retainPreState(recorder, projection, "pre_history_insert", mode);
    pushTerminalBrowserEvent(recorder, browserEvent("history_page", projection.canonical, mode, null));
    if (recorder.cycleHistoryChecked) return;
    recorder.cycleHistoryChecked = true;
    evaluateInvariants(recorder, projection, recorder.committed, false, "pre_history_insert", onTrigger);
    return;
  }
  const frame = renderer.currentFrame;
  if (!frame) return;
  const model = committedPaintedModel(projection, frame, recorder.committed);
  pushTerminalBrowserEvent(recorder, browserEvent("render_applied", frame, mode, null));
  evaluateInvariants(recorder, projection, model, true, "post_reconcile", onTrigger);
  recorder.committed = model;
  recorder.streamId = frame.streamId;
  recorder.cycleDestructive = false;
  if (recorder.trigger !== null && recorder.postRepairState === null) {
    recorder.postRepairState = capturePaintedState(recorder, projection, "post_reconcile", null);
  }
}

function retainPreState(
  recorder: TerminalIncidentRecorder,
  projection: RendererProjection,
  phase: TerminalBrowserPhase,
  mode: "full" | "delta" | null,
): void {
  recorder.preState = capturePaintedState(recorder, projection, phase, mode);
  recorder.cyclePreState = true;
}

/** A model comparison is only meaningful against a DOM the renderer owns: a
 *  reader hold, a pending canonical frame, a resize or an epoch change is a
 *  recorded boundary, not an anomaly. */
function modelComparisonSafe(
  projection: RendererProjection,
  committed: CommittedPaintedModel,
): boolean {
  return projection.holdMask === 0
    && projection.readerIntent === "live"
    && committed.gridEpoch === projection.reconciledWatermark.grid_epoch
    && committed.seq === projection.reconciledWatermark.seq
    && committed.cols === projection.paintedCols
    && committed.rows === projection.domRows
    && committed.altScreen === projection.reconciledAltScreen;
}

function evaluateInvariants(
  recorder: TerminalIncidentRecorder,
  projection: RendererProjection,
  committed: CommittedPaintedModel | null,
  checkViewport: boolean,
  phase: RendererIncidentPhase,
  onTrigger: IncidentTriggerHandler,
): void {
  const safe = committed !== null && modelComparisonSafe(projection, committed);
  const conflict = findPaintedConflict(
    projection,
    safe ? committed : null,
    safe && checkViewport,
  );
  if (!conflict) return;
  const streamId = recorder.streamId ?? projection.canonical?.streamId ?? null;
  const gridEpoch = committed?.gridEpoch
    ?? projection.reconciledWatermark.grid_epoch
    ?? projection.canonical?.gridEpoch
    ?? null;
  const admission = admitAutomaticCapture(
    recorder,
    streamId,
    gridEpoch,
    conflict.reason,
    Date.now(),
  );
  // The latch, not the generic per-kind cooldown, owns this line: a violation
  // that never heals is re-observed at every painted boundary, and one line per
  // cooldown window would bury the Tier-1 channel for as long as it persists.
  // Later occurrences of the same identity accumulate in the recorder's tally,
  // which the capture and STOP results report.
  if (admission.firstForIdentity) {
    signal("terminal.history_conflict", {
      sid: recorder.sessionId,
      cooldownKey: admission.latch,
      recording_id: recorder.recordingId,
      kind: conflict.kind,
      reason: conflict.reason,
      row: conflict.index,
      occurrences: admission.occurrences,
      stream_id: streamId,
      grid_epoch: gridEpoch,
      captured: admission.allowed,
    });
  }
  if (!admission.allowed) return;
  freezeTriggerEvidence(recorder, projection, conflict, phase, streamId, gridEpoch, admission.occurrences);
  onTrigger(recorder);
}

/** Preserve the DOM that violated the invariant, plus the state that preceded
 *  it, BEFORE any repair runs and before any network call. */
function freezeTriggerEvidence(
  recorder: TerminalIncidentRecorder,
  projection: RendererProjection,
  conflict: PaintedConflict,
  phase: RendererIncidentPhase,
  streamId: string | null,
  gridEpoch: string | null,
  occurrences: number,
): void {
  recorder.triggerState = capturePaintedState(recorder, projection, BROWSER_PHASE[phase], null);
  recorder.preRepairState = recorder.preState;
  recorder.postRepairState = null;
  recorder.trigger = {
    reason: conflict.reason as TerminalCaptureReason,
    origin: "browser",
    at_ms: Date.now(),
    stream_id: streamId,
    grid_epoch: gridEpoch,
    seq: projection.reconciledWatermark.seq === null
      ? null
      : String(projection.reconciledWatermark.seq),
    detail: conflict.kind,
    occurrence_count: occurrences,
  };
}
