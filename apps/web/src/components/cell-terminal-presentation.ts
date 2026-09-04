// Owns pane presentation readiness, reader holds, stall recovery, and notices.
// Renderer frames and viewport status feed this controller while CellTerminal
// only paints its accessors. Pointer gestures defer reconciliation escalation so
// live DOM cannot move under an active selection or scrollbar interaction.

import {
  createEffect,
  createMemo,
  createSignal,
  onCleanup,
  type Accessor,
  type Setter,
} from "solid-js";
import { diag, signal } from "@roost/shared/diag";
import type {
  LiveInteractionResult,
  RendererEpochSeq,
} from "../lib/cellRenderer.ts";
import { createTerminalSelectionGuard } from "../lib/terminalSelectionGuard.ts";
import {
  createTerminalPresentationController,
  FOREGROUND_DOM_STALL_MS,
  preservesForegroundReaderHold,
  type TerminalPresentationController,
} from "../lib/terminalPresentation.ts";
import { createOfflineWatch } from "../lib/offlineWatch.ts";
import { isPageVisible, pageVisible } from "../lib/pageVisible.ts";
import { newestOpenSessionForFolderKey } from "../store/selectors.ts";
import { folderKeyOf } from "../lib/folderKey.ts";
import {
  startAttachDiagnosis,
  type AttachDiagnosisHandle,
} from "../lib/attachDiagnosis.ts";
import type {
  BaselineProgress,
  TerminalPresentationState,
} from "../store/terminal-stream-types.ts";
import type { TerminalViewHandleStatus } from "../store/terminal-stream.ts";
import {
  terminalViewportLoadingNotice,
  type TerminalLoadingNoticeProps,
} from "./TerminalOfflineNotice.tsx";
import type { TerminalSelectionGuard } from "./TerminalComposeButton.tsx";
import type { CellTerminalProps } from "./cell-terminal-types.ts";
import type { CellTerminalRuntime } from "./cell-terminal-runtime.ts";

const OFFLINE_GRACE_MS = 3000;
const ATTACH_DIAGNOSIS_GRACE_MS = 2000;

export interface CellTerminalPresentation {
  attachPointerGestureGuard(): () => void;
  captureTerminalSelection(): TerminalSelectionGuard | undefined;
  clearCursorBlink(): void;
  dispose(): void;
  clearDomStallRecovery(): void;
  clearFrameActivity(): void;
  hasReconciledFrame: Accessor<boolean>;
  loadingNotice: Accessor<TerminalLoadingNoticeProps | null>;
  loadingProgress: Accessor<{ received: number; total: number } | null>;
  noteFrameActivity: TerminalPresentationController["noteFrameActivity"];
  notifyBackfill(result: LiveInteractionResult | undefined): void;
  offline: Accessor<boolean>;
  offlineSibling: () => { id: string } | null;
  openOfflineSibling(): void;
  prepareLiveInteraction(): void;
  presentationState: Accessor<TerminalPresentationState>;
  refreshCursorBlink(): void;
  refreshTerminalPresentation(): void;
  releasePaintHolds(): void;
  retryOffline(): void;
  setAttachProgress: Setter<BaselineProgress | null>;
  setHasReconciledFrame: Setter<boolean>;
  setViewStatus: Setter<TerminalViewHandleStatus | null>;
  setViewportLiveReady: Setter<boolean>;
  stuckReason: Accessor<string | null>;
  syncNativeSelectionHold(): void;
  viewStatus: Accessor<TerminalViewHandleStatus | null>;
  viewportLiveReady: Accessor<boolean>;
}

export function createCellTerminalPresentation(
  props: CellTerminalProps,
  runtime: CellTerminalRuntime,
  pending: Accessor<boolean>,
  viewActive: Accessor<boolean>,
  navigate: (href: string) => void,
): CellTerminalPresentation {
  const selection = createTerminalSelectionGuard({
    getDisplay: runtime.display,
    getRenderer: () => runtime.renderer,
    getBackfill: () => runtime.backfill,
    getLinkAttachment: () => runtime.linkAttachment,
  });
  const [viewportLiveReady, setViewportLiveReady] = createSignal(false);
  const [viewStatus, setViewStatus] =
    createSignal<TerminalViewHandleStatus | null>(null);
  const [hasReconciledFrame, setHasReconciledFrame] = createSignal(false);
  const pointerGestures = new Set<number>();
  let deferredDomStall: RendererEpochSeq | null = null;
  let deferredDomEscalation: RendererEpochSeq | null = null;
  let domEscalationTimer: ReturnType<typeof setTimeout> | null = null;
  let releasePointerGestureListeners = (): void => undefined;

  const protectedReaderReason = (): boolean =>
    preservesForegroundReaderHold(runtime.renderer?.readerReason ?? null);
  const foregroundViewReady = (): boolean => {
    const status = viewStatus();
    return status?.status === "accepted" && status.active && status.baselineReady;
  };
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
  const clearDomStallRecovery = (): void => {
    clearTimeout(domEscalationTimer ?? undefined);
    domEscalationTimer = null;
    pointerGestures.clear();
    releasePointerGestureListeners();
    deferredDomStall = null;
    deferredDomEscalation = null;
  };
  const escalateDomStall = (watermark: RendererEpochSeq): void => {
    if (
      !viewActive()
      || !foregroundViewReady()
      || !isPageVisible()
      || protectedReaderReason()
      || !watermarkStillUnreconciled(watermark)
    ) return;
    if (pointerGestures.size > 0) {
      deferredDomEscalation = watermark;
      return;
    }
    signal("cell.foreground_stall", {
      sid: runtime.sessionId,
      layer: "dom_reconcile",
      action: "resync",
      block_reason: runtime.renderer?.reconcileBlockReason() ?? null,
      cooldownKey: runtime.sessionId,
    });
    runtime.view?.challengeLiveness();
  };
  const armDomEscalation = (watermark: RendererEpochSeq): void => {
    clearTimeout(domEscalationTimer ?? undefined);
    domEscalationTimer = setTimeout(() => {
      domEscalationTimer = null;
      escalateDomStall(watermark);
    }, FOREGROUND_DOM_STALL_MS);
  };
  const handleCatchUpStalled = (watermark: RendererEpochSeq): void => {
    if (
      domEscalationTimer !== null
      || !viewActive()
      || !isPageVisible()
      || protectedReaderReason()
      || !foregroundViewReady()
      || !watermarkStillUnreconciled(watermark)
    ) return;
    if (pointerGestures.size > 0) {
      deferredDomStall = watermark;
      return;
    }
    signal("cell.foreground_stall", {
      sid: runtime.sessionId,
      layer: "dom_reconcile",
      action: "reconcile",
      block_reason: runtime.renderer?.reconcileBlockReason() ?? null,
      cooldownKey: runtime.sessionId,
    });
    runtime.predictor?.clear();
    runtime.renderer?.setPredictedCursor(null);
    selection.prepareLiveInteraction();
    terminalPresentation.refreshTerminalPresentation();
    runtime.view?.refresh();
    const latest = runtime.renderer?.canonicalEpochSeq();
    if (latest && watermarkStillUnreconciled(latest)) armDomEscalation(latest);
  };
  const terminalPresentation = createTerminalPresentationController({
    active: viewActive,
    focused: () => props.focused === true,
    status: viewStatus,
    renderer: () => runtime.renderer,
    onCatchUpStalled: handleCatchUpStalled,
  });
  const syncNativeSelectionHold = (): void => {
    selection.syncNativeSelectionHold();
    terminalPresentation.refreshTerminalPresentation();
  };
  createEffect(() => {
    if (!viewActive() || !pageVisible() || !foregroundViewReady()) {
      clearDomStallRecovery();
    }
  });

  const attachPointerGestureGuard = (): (() => void) => {
    const display = runtime.display();
    if (!display) return () => undefined;
    const onPointerSettled = (event: PointerEvent): void => {
      pointerGestures.delete(event.pointerId);
      if (pointerGestures.size > 0) return;
      window.removeEventListener("pointerup", onPointerSettled, true);
      window.removeEventListener("pointercancel", onPointerSettled, true);
      const stalled = deferredDomStall;
      deferredDomStall = null;
      if (stalled) handleCatchUpStalled(stalled);
      const escalation = deferredDomEscalation;
      if (
        !stalled
        && terminalPresentation.state() === "catching_up"
        && !protectedReaderReason()
      ) {
        const current = runtime.renderer?.canonicalEpochSeq();
        if (current) handleCatchUpStalled(current);
      }
      deferredDomEscalation = null;
      if (escalation) escalateDomStall(escalation);
    };
    const onPointerDown = (event: PointerEvent): void => {
      if (pointerGestures.size === 0) {
        window.addEventListener("pointerup", onPointerSettled, true);
        window.addEventListener("pointercancel", onPointerSettled, true);
      }
      pointerGestures.add(event.pointerId);
    };
    releasePointerGestureListeners = () => {
      window.removeEventListener("pointerup", onPointerSettled, true);
      window.removeEventListener("pointercancel", onPointerSettled, true);
      pointerGestures.clear();
    };
    display.addEventListener("pointerdown", onPointerDown, true);
    return () => {
      display.removeEventListener("pointerdown", onPointerDown, true);
      releasePointerGestureListeners();
    };
  };

  const [offline, setOffline] = createSignal(false);
  const retryOffline = (): void => runtime.view?.refresh();
  const offlineWatch = createOfflineWatch(OFFLINE_GRACE_MS, setOffline, () => {
    diag("cell.offline_retry", { sid: runtime.sessionId });
    retryOffline();
  });
  createEffect(() => offlineWatch.update(
    viewActive() && isPageVisible(),
    hasReconciledFrame() && viewportLiveReady(),
  ));
  const offlineSibling = () =>
    newestOpenSessionForFolderKey(folderKeyOf(props.session), runtime.sessionId);
  const openOfflineSibling = (): void => {
    const sibling = offlineSibling();
    if (sibling) navigate(`/s/${sibling.id}`);
  };

  const [attachProgress, setAttachProgress] =
    createSignal<BaselineProgress | null>(null);
  const loadingProgress = createMemo(() => {
    const progress = attachProgress();
    return progress === null
      ? null
      : { received: progress.receivedChunks, total: progress.totalChunks };
  });
  const loadingNotice = createMemo(() => {
    if (
      !viewActive()
      || !pageVisible()
      || offline()
      || (hasReconciledFrame() && viewportLiveReady())
    ) return null;
    return terminalViewportLoadingNotice(pending(), viewStatus());
  });
  const [stuckReason, setStuckReason] = createSignal<string | null>(null);
  let attachDiagnosis: AttachDiagnosisHandle | null = null;
  let attachDiagnosisTimer: ReturnType<typeof setTimeout> | null = null;
  const clearAttachDiagnosis = (): void => {
    clearTimeout(attachDiagnosisTimer ?? undefined);
    attachDiagnosisTimer = null;
    attachDiagnosis?.dispose();
    attachDiagnosis = null;
  };
  const loadingStage = createMemo(() => loadingNotice()?.stage ?? null);
  createEffect(() => {
    const waitingForWire = loadingStage() === "viewport"
      || loadingStage() === "frame";
    if (!waitingForWire) {
      setStuckReason(null);
      return;
    }
    attachDiagnosisTimer = setTimeout(() => {
      attachDiagnosisTimer = null;
      attachDiagnosis = startAttachDiagnosis(runtime.sessionId, setStuckReason);
    }, ATTACH_DIAGNOSIS_GRACE_MS);
    onCleanup(clearAttachDiagnosis);
  });
  let disposed = false;
  const dispose = (): void => {
    if (disposed) return;
    disposed = true;
    clearAttachDiagnosis();
    offlineWatch.dispose();
    clearDomStallRecovery();
    terminalPresentation.clearFrameActivity();
    terminalPresentation.clearCursorBlink();
    selection.releasePaintHolds();
  };

  return {
    attachPointerGestureGuard,
    captureTerminalSelection: selection.captureTerminalSelection,
    clearCursorBlink: terminalPresentation.clearCursorBlink,
    clearDomStallRecovery,
    dispose,
    clearFrameActivity: terminalPresentation.clearFrameActivity,
    hasReconciledFrame,
    loadingNotice,
    loadingProgress,
    noteFrameActivity: terminalPresentation.noteFrameActivity,
    notifyBackfill: selection.notifyBackfill,
    offline,
    offlineSibling,
    openOfflineSibling,
    prepareLiveInteraction: selection.prepareLiveInteraction,
    presentationState: terminalPresentation.state,
    refreshCursorBlink: terminalPresentation.refreshCursorBlink,
    refreshTerminalPresentation: terminalPresentation.refreshTerminalPresentation,
    releasePaintHolds: selection.releasePaintHolds,
    retryOffline,
    setAttachProgress,
    setHasReconciledFrame,
    setViewStatus,
    setViewportLiveReady,
    stuckReason,
    syncNativeSelectionHold,
    viewStatus,
    viewportLiveReady,
  };
}
