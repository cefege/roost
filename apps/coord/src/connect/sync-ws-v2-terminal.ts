// Owns terminal ingress, scoped materialization discard, and canonical snapshot
// admission for one Sync-v2 socket. Ready-ring turns and delivered-head advance
// live in sync-ws-v2-terminal-ready.ts so idle terminal lanes never enter egress.
// The terminal screen hub remains the sole source of every replacement full.

import type { ServerWebSocket } from "bun";
import {
  SyncDomain,
  type FirehoseFrame,
} from "@roost/shared/proto/sync_pb";
import { log } from "@roost/shared/log";
import type { SyncFeedFrameMeta } from "./sync-feed.ts";
import type { SyncWsData } from "./sync-ws-handler.ts";
import {
  V2_TERMINAL_LANE_MAX_DELTA_BYTES,
  V2_TERMINAL_LANE_MAX_DELTA_FRAMES,
  releaseV2AggregateFrame,
  releaseV2TerminalCursor,
  releaseV2TerminalDeltaTail,
  releaseV2TerminalLane,
  tryRetainV2AggregateFrame,
  ownV2ApplicationFrame,
  type SyncTerminalSessionLane,
  type SyncV2RetainedFrame,
} from "./sync-ws-v2-state.ts";
import { makeSyncV2TerminalReadyScheduler } from "./sync-ws-v2-terminal-ready.ts";

interface SyncV2TerminalSchedulerDeps {
  enqueueRetainedV2Frame(
    ws: ServerWebSocket<SyncWsData>,
    retained: SyncV2RetainedFrame,
    meta: SyncFeedFrameMeta,
  ): boolean;
  removeTerminalQueued(
    ws: ServerWebSocket<SyncWsData>,
    sessionId: string,
    includeState: boolean,
  ): void;
  requestTerminalRebaseline?(
    ws: ServerWebSocket<SyncWsData>,
    sessionId: string,
  ): boolean;
}

export function makeSyncV2TerminalScheduler(
  deps: SyncV2TerminalSchedulerDeps,
) {
  const {
    enqueueRetainedV2Frame,
    removeTerminalQueued,
    requestTerminalRebaseline = () => false,
  } = deps;
  const ready = makeSyncV2TerminalReadyScheduler({
    enqueueRetainedV2Frame,
    requestTerminalRebaseline,
  });

  function retainTerminalFrames(
    ws: ServerWebSocket<SyncWsData>,
    frames: readonly FirehoseFrame[],
  ): SyncV2RetainedFrame[] | null {
    const v2 = ws.data.v2;
    const terminal = v2?.domains.get(SyncDomain.TERMINAL);
    if (!v2 || !terminal) return null;
    const retained: SyncV2RetainedFrame[] = [];
    for (const frame of frames) {
      const item = tryRetainV2AggregateFrame(
        v2,
        ownV2ApplicationFrame(frame, SyncDomain.TERMINAL, terminal.generation),
      );
      if (item) {
        retained.push(item);
        continue;
      }
      for (const staged of retained) releaseV2AggregateFrame(v2, staged);
      return null;
    }
    return retained;
  }

  function discardUnsentMaterialization(
    ws: ServerWebSocket<SyncWsData>,
    sessionId: string,
    lane: SyncTerminalSessionLane,
  ): void {
    const v2 = ws.data.v2;
    const cursor = lane.cursor;
    if (!v2 || !cursor) {
      removeTerminalQueued(ws, sessionId, false);
      return;
    }
    const activeSnapshot = cursor.index > 0 && cursor.index < cursor.frames.length;
    if (activeSnapshot) {
      releaseV2TerminalDeltaTail(v2, cursor);
      return;
    }
    removeTerminalQueued(ws, sessionId, false);
    releaseV2TerminalCursor(v2, lane);
  }

  function requestScopedRebaseline(
    ws: ServerWebSocket<SyncWsData>,
    sessionId: string,
    lane: SyncTerminalSessionLane,
    reason: string,
  ): void {
    const v2 = ws.data.v2;
    if (!v2) return;
    if (!lane.rebaselinePending) {
      log.warn("sync-ws", "terminal_lane_rebaseline", { session_id: sessionId, reason });
    }
    lane.rebaselinePending = true;
    discardUnsentMaterialization(ws, sessionId, lane);
    ready.markReady(v2, sessionId, lane);
  }

  const clearSessions = (ws: ServerWebSocket<SyncWsData>): void => {
    const v2 = ws.data.v2;
    if (!v2) return;
    for (const lane of v2.terminalSessions.values()) releaseV2TerminalLane(v2, lane);
    v2.terminalReadySessions.clear();
    v2.terminalSessions.clear();
  };

  const deleteSession = (
    ws: ServerWebSocket<SyncWsData>,
    sessionId: string,
  ): void => {
    const v2 = ws.data.v2;
    const lane = v2?.terminalSessions.get(sessionId);
    if (!v2 || !lane) return;
    releaseV2TerminalLane(v2, lane);
    v2.terminalReadySessions.delete(sessionId);
    v2.terminalSessions.delete(sessionId);
  };

  const beginTerminalStream = (
    ws: ServerWebSocket<SyncWsData>,
    sessionId: string,
    streamId: string,
  ): boolean => {
    const v2 = ws.data.v2;
    if (!v2) return false;
    let lane = v2.terminalSessions.get(sessionId);
    if (lane?.streamId === streamId) return false;
    if (!lane) {
      lane = {
        streamId,
        cursor: null,
        pendingStates: [],
        stateQueued: false,
        ready: false,
        rebaselinePending: false,
        attachPriorityPending: true,
      };
      v2.terminalSessions.set(sessionId, lane);
    } else {
      v2.terminalReadySessions.delete(sessionId);
      lane.ready = false;
      removeTerminalQueued(ws, sessionId, false);
      releaseV2TerminalCursor(v2, lane);
      lane.streamId = streamId;
      lane.rebaselinePending = false;
      lane.attachPriorityPending = true;
    }
    if (lane.pendingStates.length > 0 && !lane.stateQueued) {
      ready.markReady(v2, sessionId, lane);
      ready.pump(ws, sessionId);
    }
    return true;
  };

  const enqueueTerminalState = (
    ws: ServerWebSocket<SyncWsData>,
    frame: FirehoseFrame,
    sessionId: string,
  ): void => {
    const v2 = ws.data.v2;
    if (!v2?.domains.has(SyncDomain.TERMINAL)) return;
    let lane = v2.terminalSessions.get(sessionId);
    if (!lane) {
      lane = {
        streamId: "",
        cursor: null,
        pendingStates: [],
        stateQueued: false,
        ready: false,
        rebaselinePending: false,
        attachPriorityPending: false,
      };
      v2.terminalSessions.set(sessionId, lane);
    }
    const retained = retainTerminalFrames(ws, [frame]);
    if (!retained) {
      requestScopedRebaseline(ws, sessionId, lane, "terminal_aggregate_pressure");
      return;
    }
    lane.pendingStates.push(retained[0]!);
    ready.markReady(v2, sessionId, lane);
    ready.pump(ws, sessionId);
  };

  const replaceTerminalSnapshot = (
    ws: ServerWebSocket<SyncWsData>,
    sessionId: string,
    streamId: string,
    frames: readonly FirehoseFrame[],
  ): boolean => {
    const v2 = ws.data.v2;
    const lane = v2?.terminalSessions.get(sessionId);
    if (!v2 || !lane || lane.streamId !== streamId || frames.length === 0) return false;
    const cursor = lane.cursor;
    if (cursor && cursor.index > 0 && cursor.index < cursor.frames.length) {
      lane.rebaselinePending = true;
      ready.markReady(v2, sessionId, lane);
      return false;
    }
    discardUnsentMaterialization(ws, sessionId, lane);
    const retained = retainTerminalFrames(ws, frames);
    if (!retained) {
      lane.rebaselinePending = true;
      ready.markReady(v2, sessionId, lane);
      return false;
    }
    lane.cursor = {
      streamId,
      frames: retained,
      index: 0,
      queued: false,
      deltaTail: [],
      deltaBytes: 0,
    };
    lane.rebaselinePending = false;
    ready.markReady(v2, sessionId, lane);
    ready.pump(ws, sessionId);
    return true;
  };

  const enqueueTerminalDelta = (
    ws: ServerWebSocket<SyncWsData>,
    sessionId: string,
    streamId: string,
    frame: FirehoseFrame,
  ): boolean => {
    const v2 = ws.data.v2;
    const lane = v2?.terminalSessions.get(sessionId);
    if (!v2 || !lane || lane.streamId !== streamId || lane.rebaselinePending) return false;
    let cursor = lane.cursor;
    if (!cursor) {
      cursor = {
        streamId,
        frames: [],
        index: 0,
        queued: false,
        deltaTail: [],
        deltaBytes: 0,
      };
      lane.cursor = cursor;
    }
    if (cursor.deltaTail.length + 1 > V2_TERMINAL_LANE_MAX_DELTA_FRAMES) {
      requestScopedRebaseline(ws, sessionId, lane, "terminal_delta_lane_pressure");
      return false;
    }
    const retained = retainTerminalFrames(ws, [frame]);
    if (!retained) {
      requestScopedRebaseline(ws, sessionId, lane, "terminal_aggregate_pressure");
      return false;
    }
    const item = retained[0]!;
    if (cursor.deltaBytes + item.estimatedBytes > V2_TERMINAL_LANE_MAX_DELTA_BYTES) {
      releaseV2AggregateFrame(v2, item);
      requestScopedRebaseline(ws, sessionId, lane, "terminal_delta_lane_pressure");
      return false;
    }
    cursor.deltaTail.push({ ...item, payloadBytes: item.estimatedBytes });
    cursor.deltaBytes += item.estimatedBytes;
    ready.markReady(v2, sessionId, lane);
    ready.pump(ws, sessionId);
    return true;
  };

  const terminalRebaselinePending = (
    ws: ServerWebSocket<SyncWsData>,
    sessionId: string,
    streamId: string,
  ): boolean => {
    const lane = ws.data.v2?.terminalSessions.get(sessionId);
    return lane?.streamId === streamId && lane.rebaselinePending === true;
  };

  const dropTerminalSession = (
    ws: ServerWebSocket<SyncWsData>,
    sessionId: string,
  ): void => {
    const v2 = ws.data.v2;
    const lane = v2?.terminalSessions.get(sessionId);
    if (!v2 || !lane) return;
    removeTerminalQueued(ws, sessionId, false);
    releaseV2TerminalLane(v2, lane);
    v2.terminalReadySessions.delete(sessionId);
    v2.terminalSessions.delete(sessionId);
  };

  return {
    clearSessions,
    deleteSession,
    onFrameDelivered: ready.onFrameDelivered,
    onEgressProgress: (ws: ServerWebSocket<SyncWsData>) => ready.pump(ws),
    beginTerminalStream,
    enqueueTerminalState,
    replaceTerminalSnapshot,
    enqueueTerminalDelta,
    terminalRebaselinePending,
    dropTerminalSession,
  };
}
