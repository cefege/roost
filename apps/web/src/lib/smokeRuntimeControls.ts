// Browser smoke scenarios need controlled transport faults and direct runtime observations.
// These methods expose existing Sync, renderer, visibility, and performance instrumentation.
// The smoke backdoor delegates here instead of duplicating production state or mutation logic.
// Every control remains inert until the gated smoke API is installed.

import { phaseTimeline as phaseTimelineImpl } from "./diag.ts";
import { leakSample, perfCounters, resetPerfCounters as resetPerfCountersImpl } from "./leakWatch.ts";
import { setForceHidden, setForceVisible } from "./pageVisible.ts";
import { scrollbackBackfillRequestCount as scrollbackBackfillRequestCountImpl } from "./scrollbackBackfill.ts";
import type { SmokeApi } from "./smokeTypes.ts";
import { rootStore } from "../store/root.ts";
import {
  cellFrameCount as cellFrameCountImpl,
  cellFullFrameCount as cellFullFrameCountImpl,
  cellGridEpoch as cellGridEpochImpl,
  forceSyncMaxBackoff as forceSyncMaxBackoffImpl,
  lastFullFrameSbRows as lastFullFrameSbRowsImpl,
  pauseSyncTransport as pauseSyncTransportImpl,
  resumeSyncTransport as resumeSyncTransportImpl,
  syncRedialStatus as syncRedialStatusImpl,
  syncWsGeneration as syncWsGenerationImpl,
} from "../store/sync.ts";
import {
  blackholeTerminalFramesForCurrentGeneration as blackholeTerminalFramesImpl,
  dropNextCellFrame as dropNextCellFrameImpl,
  dropNextTerminalWireDelta as dropNextTerminalWireDeltaImpl,
  droppedCellFrameCount as droppedCellFrameCountImpl,
} from "../store/terminal-stream-diagnostics.ts";

type SmokeRuntimeControlMethods = Pick<
  SmokeApi,
  | "phaseTimeline"
  | "state"
  | "forceSyncMaxBackoff"
  | "syncRedialStatus"
  | "pauseSyncTransport"
  | "resumeSyncTransport"
  | "cellFrameCount"
  | "cellFullFrameCount"
  | "lastFullFrameSbRows"
  | "scrollbackBackfillRequestCount"
  | "cellGridEpoch"
  | "blackholeTerminalFramesForCurrentGeneration"
  | "dropNextTerminalWireDelta"
  | "dropNextCellFrame"
  | "droppedCellFrameCount"
  | "perfProbe"
  | "resetPerfCounters"
  | "syncWsGeneration"
  | "forceVisible"
  | "forceHidden"
  | "navigate"
>;

export function createSmokeRuntimeControlMethods(): SmokeRuntimeControlMethods {
  return {
    phaseTimeline() {
      return phaseTimelineImpl();
    },
    state() {
      return {
        sessions: { ...rootStore.sessions },
        workspaces: { ...rootStore.workspaces },
        workers: { ...rootStore.workers },
        pair_requests: { ...rootStore.pair_requests },
      };
    },
    forceSyncMaxBackoff() {
      forceSyncMaxBackoffImpl();
    },
    syncRedialStatus() {
      return syncRedialStatusImpl();
    },
    pauseSyncTransport() {
      pauseSyncTransportImpl();
    },
    resumeSyncTransport() {
      resumeSyncTransportImpl();
    },
    cellFrameCount(sessionId) {
      return cellFrameCountImpl(sessionId);
    },
    cellFullFrameCount(sessionId) {
      return cellFullFrameCountImpl(sessionId);
    },
    lastFullFrameSbRows(sessionId) {
      return lastFullFrameSbRowsImpl(sessionId);
    },
    scrollbackBackfillRequestCount(sessionId) {
      return scrollbackBackfillRequestCountImpl(sessionId);
    },
    cellGridEpoch(sessionId) {
      return cellGridEpochImpl(sessionId);
    },
    blackholeTerminalFramesForCurrentGeneration(sessionId) {
      blackholeTerminalFramesImpl(sessionId);
    },
    dropNextTerminalWireDelta(sessionId) {
      dropNextTerminalWireDeltaImpl(sessionId);
    },
    dropNextCellFrame(sessionId) {
      dropNextCellFrameImpl(sessionId);
    },
    droppedCellFrameCount(sessionId) {
      return droppedCellFrameCountImpl(sessionId);
    },
    perfProbe(sessionId) {
      const counters = perfCounters();
      const sample = leakSample();
      return {
        longTaskState: counters.longTaskState,
        longTaskCount: counters.longTaskCount,
        longTaskMs: counters.longTaskMs,
        cellFrames: cellFrameCountImpl(sessionId),
        cellFullFrames: cellFullFrameCountImpl(sessionId),
        domNodes: sample.dom_nodes ?? -1,
        cellRows: sample.cell_rows ?? -1,
        heldSbRows: sample.held_sb_rows ?? -1,
        heapMb: sample.heap_mb ?? -1,
        inputRttP50: sample.input_rtt_p50 ?? -1,
        inputRttP95: sample.input_rtt_p95 ?? -1,
      };
    },
    resetPerfCounters() {
      resetPerfCountersImpl();
    },
    syncWsGeneration() {
      return syncWsGenerationImpl();
    },
    forceVisible(on) {
      setForceVisible(on);
    },
    forceHidden(on) {
      setForceHidden(on);
    },
    navigate(href) {
      window.dispatchEvent(new CustomEvent("roost-smoke-navigate", { detail: href }));
    },
  };
}
