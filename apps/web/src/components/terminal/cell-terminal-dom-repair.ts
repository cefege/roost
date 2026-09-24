// Pane-local DOM reconciliation repair for one terminal pane: arms a proof
// deadline against the canonical watermark the DOM has not reached, escalates
// to a view redial when that deadline expires, and defers both while a pointer
// gesture could move live DOM under the reader's hand.
// cell-terminal-presentation.ts owns one instance per pane and injects the
// pane's runtime plus its hold and visibility predicates; nothing here reads
// Solid state, so the whole escalation path is drivable from a plain test.

import { signal } from "@roost/observability/diag";
import type { RendererEpochSeq } from "../../renderer/cellRenderer.ts";
import type { TerminalPresentationState } from "../../store/terminal-stream-types.ts";
import { terminalStreamDiagnosticSnapshot } from "../../store/terminal-stream.ts";
import type { CellTerminalRuntime } from "./cell-terminal-runtime.ts";

const DOM_RECONCILIATION_PROOF_MS = 3_000;

export interface CellTerminalDomRepairOptions {
  readonly runtime: CellTerminalRuntime;
  /** The pane is in layout and its tab is foregrounded. */
  activelyViewed(): boolean;
  foregroundViewReady(): boolean;
  readerHoldActive(): boolean;
  pointerGestureActive(): boolean;
  presentationState(): TerminalPresentationState;
  prepareLiveInteraction(): void;
  refreshTerminalPresentation(): void;
}

export interface CellTerminalDomRepair {
  /** The renderer reconciled: retire a target the DOM has now reached. */
  noteReconciled(): void;
  handleCatchUpStalled(watermark: RendererEpochSeq): void;
  /** The pane's last pointer gesture settled: run whatever it deferred. */
  resumeAfterPointerGesture(): void;
  clearDomStallRecovery(): void;
}

export function createCellTerminalDomRepair(
  options: CellTerminalDomRepairOptions,
): CellTerminalDomRepair {
  const runtime = options.runtime;
  let deferredDomStall: RendererEpochSeq | null = null;
  let deferredDomEscalation: RendererEpochSeq | null = null;
  let domReconciliationWatermark: RendererEpochSeq | null = null;
  let domEscalationTimer: Timer | null = null;

  const watermarkStillUnreconciled = (watermark: RendererEpochSeq): boolean => {
    const renderer = runtime.renderer;
    if (!renderer || watermark.grid_epoch === null || watermark.seq === null) return false;
    const canonical = renderer.canonicalEpochSeq();
    const reconciled = renderer.reconciledEpochSeq();
    return canonical.grid_epoch === watermark.grid_epoch
      && canonical.seq !== null
      && canonical.seq >= watermark.seq
      && (
        reconciled.grid_epoch !== watermark.grid_epoch
        || reconciled.seq === null
        || reconciled.seq < watermark.seq
      );
  };
  const reconciledWatermarkReached = (watermark: RendererEpochSeq): boolean => {
    const reconciled = runtime.renderer?.reconciledEpochSeq();
    return reconciled?.grid_epoch === watermark.grid_epoch
      && reconciled.seq !== null
      && watermark.seq !== null
      && reconciled.seq >= watermark.seq;
  };
  const clearDomReconciliationTarget = (): void => {
    clearTimeout(domEscalationTimer ?? undefined);
    domEscalationTimer = null;
    domReconciliationWatermark = null;
    deferredDomEscalation = null;
  };
  /** One predicate for both repair gates: the stall gate and the proof deadline
   *  admit a repair on exactly these facts and must not drift apart. */
  const domRepairStillWarranted = (watermark: RendererEpochSeq): boolean =>
    options.activelyViewed() && options.foregroundViewReady()
    && !options.readerHoldActive() && watermarkStillUnreconciled(watermark);
  const recoverUnreconciledDom = (watermark: RendererEpochSeq): void => {
    // A superseded callback must not release the live owner's target.
    if (domReconciliationWatermark !== watermark) return;
    // A decline leaves no armed target behind: the stall gate reads
    // target-presence as "recovery owns this repair", so a target whose timer
    // has already fired would retire the pane's only repair for good.
    if (!domRepairStillWarranted(watermark)) {
      clearDomReconciliationTarget();
      return;
    }
    if (options.pointerGestureActive()) {
      deferredDomEscalation = watermark;
      return;
    }
    const renderer = runtime.renderer;
    const reconciled = renderer?.reconciledEpochSeq()
      ?? { grid_epoch: null, seq: null };
    const stream = terminalStreamDiagnosticSnapshot(
      runtime.sessionId,
      runtime.view?.viewId,
    );
    signal("cell.foreground_stall", {
      sid: runtime.sessionId,
      stream_id: stream.view.stream_id,
      view_revision: stream.view.revision,
      generation_socket: stream.sync.socket_generation,
      generation_domain: stream.sync.domain_generation,
      checkpoint_epoch: watermark.grid_epoch,
      checkpoint_seq: watermark.seq,
      replica_epoch: stream.replica.grid_epoch,
      replica_seq: stream.replica.seq,
      dom_reconciled_epoch: reconciled.grid_epoch,
      dom_reconciled_seq: reconciled.seq,
      reader_reason: renderer?.readerReason ?? null,
      block_reason: renderer?.reconcileBlockReason() ?? null,
      layer: "dom_reconcile",
      action: "redial",
      cooldownKey: runtime.sessionId,
    });
    runtime.view?.recoverUnreconciledDom();
  };
  const armDomReconciliationTarget = (watermark: RendererEpochSeq): void => {
    if (!watermarkStillUnreconciled(watermark)) return;
    if (
      domReconciliationWatermark?.grid_epoch === watermark.grid_epoch
      && domReconciliationWatermark.seq !== null
      && watermark.seq !== null
      && domReconciliationWatermark.seq <= watermark.seq
    ) return;
    clearDomReconciliationTarget();
    const captured = { ...watermark };
    domReconciliationWatermark = captured;
    const timer = setTimeout(() => {
      if (
        domEscalationTimer !== timer
        || domReconciliationWatermark !== captured
      ) return;
      domEscalationTimer = null;
      recoverUnreconciledDom(captured);
    }, DOM_RECONCILIATION_PROOF_MS);
    domEscalationTimer = timer;
  };
  const handleCatchUpStalled = (watermark: RendererEpochSeq): void => {
    if (domReconciliationWatermark !== null) return;
    if (!domRepairStillWarranted(watermark)) return;
    if (options.pointerGestureActive()) {
      deferredDomStall = watermark;
      return;
    }
    armDomReconciliationTarget(watermark);
    if (domReconciliationWatermark === null) return;
    runtime.predictor?.clear();
    runtime.renderer?.setPredictedCursor(null);
    options.prepareLiveInteraction();
    options.refreshTerminalPresentation();
    runtime.view?.refresh();
  };
  const resumeAfterPointerGesture = (): void => {
    const stalled = deferredDomStall;
    deferredDomStall = null;
    if (stalled) handleCatchUpStalled(stalled);
    const escalation = deferredDomEscalation;
    if (
      !stalled
      && options.presentationState() === "catching_up"
      && !options.readerHoldActive()
    ) {
      const current = runtime.renderer?.canonicalEpochSeq();
      if (current) handleCatchUpStalled(current);
    }
    deferredDomEscalation = null;
    if (escalation) recoverUnreconciledDom(escalation);
  };
  const noteReconciled = (): void => {
    const watermark = domReconciliationWatermark;
    if (watermark && reconciledWatermarkReached(watermark)) clearDomReconciliationTarget();
  };
  const clearDomStallRecovery = (): void => {
    clearDomReconciliationTarget();
    deferredDomStall = null;
  };

  return {
    noteReconciled,
    handleCatchUpStalled,
    resumeAfterPointerGesture,
    clearDomStallRecovery,
  };
}
