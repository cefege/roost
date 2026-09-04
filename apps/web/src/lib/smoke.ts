// Browser smoke scenarios need a gated API for real transport, renderer, and resource checks.
// App.tsx imports this entrypoint only when the build and local-storage smoke gates allow it.
// The entrypoint assembles focused controllers while preserving the window.__smoke contract.
// Shared types remain separate so harness consumers never import this browser implementation.

import { createSmokeCreatedResourceMethods } from "./smokeCreatedResources.ts";
import { createSmokeFileTransferMethods } from "./smokeFileTransferProbes.ts";
import {
  beginTerminalTiming as beginTerminalTimingImpl,
  finishTerminalTiming as finishTerminalTimingImpl,
  runFlow as runFlowImpl,
  runRenderStress as runRenderStressImpl,
  waitForPaintedCursor as waitForPaintedCursorImpl,
  waitForPaintedMarker as waitForPaintedMarkerImpl,
} from "./smokeHarness.ts";
import { createSmokeRetainedMarkerMethods } from "./smokeRetainedMarkerScan.ts";
import { createSmokeRuntimeControlMethods } from "./smokeRuntimeControls.ts";
import { createSmokeTerminalInputMethods } from "./smokeTerminalInputController.ts";
import { createSmokeTerminalRenderMethods } from "./smokeTerminalRenderProbes.ts";
import { createSmokeTerminalStreamProbeMethods } from "./smokeTerminalStreamProbe.ts";
import type { SmokeApi } from "./smokeTypes.ts";

export type {
  PaintedCursorProof,
  RetainedMarkerScan,
  SmokeApi,
  SmokeTerminalInputBatch,
  SmokeTerminalInputCapture,
  TerminalStreamProbe,
} from "./smokeTypes.ts";

export function maybeInstallSmokeBackdoor(): void {
  if (typeof window === "undefined") return;
  if (typeof localStorage === "undefined" || localStorage.getItem("roostSmoke") !== "1") return;

  const createdResources = createSmokeCreatedResourceMethods();
  const terminalInput = createSmokeTerminalInputMethods();
  const terminalRender = createSmokeTerminalRenderMethods();
  const terminalStreamProbe = createSmokeTerminalStreamProbeMethods();
  const retainedMarkers = createSmokeRetainedMarkerMethods();
  const runtimeControls = createSmokeRuntimeControlMethods();
  const fileTransfer = createSmokeFileTransferMethods();

  const api: SmokeApi = {
    cleanupCreated: createdResources.cleanupCreated,
    async runFlow(options) {
      return runFlowImpl(api, options);
    },
    async runRenderStress(options) {
      return runRenderStressImpl(api, options);
    },
    async waitForPaintedMarker(sessionId, marker, timeoutMs) {
      return waitForPaintedMarkerImpl(sessionId, marker, timeoutMs);
    },
    async waitForPaintedCursor(sessionId, expected, timeoutMs) {
      return waitForPaintedCursorImpl(sessionId, expected, timeoutMs);
    },
    terminalStreamProbe: terminalStreamProbe.terminalStreamProbe,
    async beginTerminalTiming(kind, sessionId) {
      return beginTerminalTimingImpl(kind, sessionId);
    },
    async finishTerminalTiming(timingId, sessionId, marker, timeoutMs) {
      return finishTerminalTimingImpl(timingId, sessionId, marker, timeoutMs);
    },
    phaseTimeline: runtimeControls.phaseTimeline,
    retainedMarkerScan: retainedMarkers.retainedMarkerScan,
    input: terminalInput.input,
    terminalInputCapture: terminalInput.terminalInputCapture,
    resetTerminalInputCapture: terminalInput.resetTerminalInputCapture,
    paneFocused: terminalRender.paneFocused,
    viewportText: terminalRender.viewportText,
    renderProbe: terminalRender.renderProbe,
    paintedScrollback: terminalRender.paintedScrollback,
    markerScan: terminalRender.markerScan,
    terminalDimensions: terminalRender.terminalDimensions,
    state: runtimeControls.state,
    forceSyncMaxBackoff: runtimeControls.forceSyncMaxBackoff,
    syncRedialStatus: runtimeControls.syncRedialStatus,
    pauseSyncTransport: runtimeControls.pauseSyncTransport,
    resumeSyncTransport: runtimeControls.resumeSyncTransport,
    cellFrameCount: runtimeControls.cellFrameCount,
    cellFullFrameCount: runtimeControls.cellFullFrameCount,
    lastFullFrameSbRows: runtimeControls.lastFullFrameSbRows,
    scrollbackBackfillRequestCount: runtimeControls.scrollbackBackfillRequestCount,
    cellGridEpoch: runtimeControls.cellGridEpoch,
    blackholeTerminalFramesForCurrentGeneration:
      runtimeControls.blackholeTerminalFramesForCurrentGeneration,
    dropNextTerminalWireDelta: runtimeControls.dropNextTerminalWireDelta,
    dropNextCellFrame: runtimeControls.dropNextCellFrame,
    droppedCellFrameCount: runtimeControls.droppedCellFrameCount,
    perfProbe: runtimeControls.perfProbe,
    resetPerfCounters: runtimeControls.resetPerfCounters,
    syncWsGeneration: runtimeControls.syncWsGeneration,
    forceVisible: runtimeControls.forceVisible,
    forceHidden: runtimeControls.forceHidden,
    navigate: runtimeControls.navigate,
    kill: createdResources.kill,
    spawnShell: createdResources.spawnShell,
    trackCreatedSession: createdResources.trackCreatedSession,
    createWorkspace: createdResources.createWorkspace,
    uploadAttachment: fileTransfer.uploadAttachment,
    attachmentProbe: fileTransfer.attachmentProbe,
    downloadWorkerFile: fileTransfer.downloadWorkerFile,
  };
  (window as Window & { __smoke?: SmokeApi }).__smoke = api;
  console.debug("[smoke] backdoor installed via window.__smoke");
}
