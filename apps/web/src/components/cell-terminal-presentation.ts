// Owns pane presentation readiness, reader holds, view liveness, and notices.
// Renderer frames and viewport status feed this controller while CellTerminal
// only paints its accessors. DOM stall recovery lives in
// cell-terminal-dom-repair.ts, driven from here with the pane's hold,
// visibility and pointer-gesture facts so live DOM cannot move under an active
// selection or scrollbar interaction.

import {
  createEffect,
  createMemo,
  createSignal,
  onCleanup,
  type Accessor,
  type Setter,
} from "solid-js";
import { diag } from "@roost/shared/diag";
import type { LiveInteractionResult } from "../lib/cellRenderer.ts";
import { createTerminalSelectionGuard } from "../lib/terminalSelectionGuard.ts";
import {
  createTerminalPresentationController,
  preservesForegroundReaderHold,
  type TerminalPresentationController,
} from "../lib/terminalPresentation.ts";
import { createOfflineWatch } from "../lib/offlineWatch.ts";
import { isPageVisible, pageVisible } from "../lib/pageVisible.ts";
import { newestOpenSessionForFolderKey } from "../store/selectors.ts";
import { folderKeyOf } from "../lib/folderKey.ts";
import {
  attachDiagnosisWaitKey,
  startAttachDiagnosis,
  type AttachDiagnosisHandle,
} from "../lib/attachDiagnosis.ts";
import {
  FRAME_ACTIVITY_WINDOW_MS,
  type BaselineProgress,
  type TerminalPresentationState,
} from "../store/terminal-stream-types.ts";
import type { TerminalViewHandleStatus } from "../store/terminal-stream.ts";
import {
  terminalViewportLoadingNotice,
  type TerminalLoadingNoticeProps,
} from "./TerminalOfflineNotice.tsx";
import type { TerminalSelectionGuard } from "./TerminalComposeButton.tsx";
import { createCellTerminalDomRepair } from "./cell-terminal-dom-repair.ts";
import type { CellTerminalProps } from "./cell-terminal-types.ts";
import type { CellTerminalRuntime } from "./cell-terminal-runtime.ts";

const OFFLINE_GRACE_MS = 3000;
const ATTACH_DIAGNOSIS_GRACE_MS = 3_000;

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
  noteRendererReconciled(): void;
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
  stuckReason: Accessor<string | null>;
  syncNativeSelectionHold(): void;
  viewStatus: Accessor<TerminalViewHandleStatus | null>;
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
  const [viewStatus, setViewStatus] =
    createSignal<TerminalViewHandleStatus | null>(null);
  const [hasReconciledFrame, setHasReconciledFrame] = createSignal(false);
  const pointerGestures = new Set<number>();
  let lastFramePaintedAtMs: number | null = null;
  let releasePointerGestureListeners = (): void => undefined;

  const protectedReaderReason = (): boolean =>
    preservesForegroundReaderHold(runtime.renderer?.readerReason ?? null);
  const foregroundViewReady = (): boolean => {
    const status = viewStatus();
    return status?.status === "accepted" && status.active && status.baselineReady;
  };
  const retainsReconciledFrameDuringRefresh = (): boolean => {
    const status = viewStatus();
    return hasReconciledFrame()
      && (
        status?.status === "pending"
        || (status?.status === "accepted" && !status.baselineReady)
      );
  };
  const domRepair = createCellTerminalDomRepair({
    runtime,
    activelyViewed: () => viewActive() && isPageVisible(),
    foregroundViewReady,
    readerHoldActive: protectedReaderReason,
    pointerGestureActive: () => pointerGestures.size > 0,
    presentationState: () => terminalPresentation.state(),
    prepareLiveInteraction: selection.prepareLiveInteraction,
    refreshTerminalPresentation: () => terminalPresentation.refreshTerminalPresentation(),
  });
  const terminalPresentation = createTerminalPresentationController({
    active: viewActive,
    focused: () => props.focused === true,
    status: viewStatus,
    renderer: () => runtime.renderer,
    onCatchUpStalled: domRepair.handleCatchUpStalled,
  });
  const noteRendererReconciled = (): void => {
    lastFramePaintedAtMs = Date.now();
    domRepair.noteReconciled();
    terminalPresentation.refreshTerminalPresentation();
    // A painted frame is proof this view delivers. The status it contradicts
    // may never change again, so the retraction rides the frame rather than
    // waiting for an event a detached pane has already stopped producing.
    if (offline()) refreshOfflineWatch();
  };
  const syncNativeSelectionHold = (): void => {
    selection.syncNativeSelectionHold();
    terminalPresentation.refreshTerminalPresentation();
  };
  createEffect(() => {
    if (!pageVisible()) releasePointerGestureListeners();
    if (!viewActive() || !pageVisible() || !foregroundViewReady()) {
      domRepair.clearDomStallRecovery();
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
      domRepair.resumeAfterPointerGesture();
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
  /** A frame painted inside this window proves the view still delivers, so no
   *  accusation may stand. The window is SHORTER than DETACHED_GRACE_MS, which
   *  a view must outlast before it reads detached: freshness therefore cannot
   *  still be true at the edge that arms the re-claim and mask it. */
  const framePaintedRecently = (): boolean =>
    lastFramePaintedAtMs !== null
    && Date.now() - lastFramePaintedAtMs < FRAME_ACTIVITY_WINDOW_MS;
  /** Output silence is never evidence — a quiet shell prints nothing for hours.
   *  Only a view the operator is looking at that has stopped being deliverable
   *  may be re-claimed, and `detached` is exactly that fact. */
  const refreshOfflineWatch = (): void => offlineWatch.update(
    viewStatus() !== null && viewActive() && isPageVisible(),
    terminalPresentation.state() === "detached",
    framePaintedRecently(),
  );
  createEffect(refreshOfflineWatch);
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
      || (hasReconciledFrame() && foregroundViewReady())
      || retainsReconciledFrameDuringRefresh()
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
    setStuckReason(null);
  };
  const loadingStage = createMemo(() => loadingNotice()?.stage ?? null);
  createEffect(() => {
    // Every accepted chunk changes this key, restarting diagnosis grace.
    const waitKey = attachDiagnosisWaitKey(loadingStage(), attachProgress());
    if (waitKey === null) {
      clearAttachDiagnosis();
      return;
    }
    // Diagnosis is advisory; liveness and scoped repair do not wait for it.
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
    domRepair.clearDomStallRecovery();
    releasePointerGestureListeners();
    terminalPresentation.clearFrameActivity();
    terminalPresentation.clearCursorBlink();
    selection.releasePaintHolds();
  };

  return {
    attachPointerGestureGuard,
    captureTerminalSelection: selection.captureTerminalSelection,
    clearCursorBlink: terminalPresentation.clearCursorBlink,
    clearDomStallRecovery: domRepair.clearDomStallRecovery,
    dispose,
    clearFrameActivity: terminalPresentation.clearFrameActivity,
    hasReconciledFrame,
    loadingNotice,
    loadingProgress,
    noteFrameActivity: terminalPresentation.noteFrameActivity,
    noteRendererReconciled,
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
    stuckReason,
    syncNativeSelectionHold,
    viewStatus,
  };
}
