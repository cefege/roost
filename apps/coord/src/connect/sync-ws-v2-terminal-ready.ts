// Owns the deduplicated per-socket terminal ready ring and one-head materialization.
// Terminal ingress retains frames before waking a lane; egress calls this owner only
// for the delivered lane or one deferred retry, never by scanning all sessions.
// It depends on terminal-lane state and the shared cross-domain queue admission seam.

import type { ServerWebSocket } from "bun";
import { SyncDomain } from "@roost/shared/proto/sync_pb";
import type { SyncFeedFrameMeta } from "./sync-feed.ts";
import type { SyncWsData } from "./sync-ws-handler.ts";
import {
  isV2SnapshotFrame,
  type SyncTerminalSessionLane,
  type SyncV2RetainedFrame,
} from "./sync-ws-v2-state.ts";

export interface SyncV2TerminalReadySchedulerDeps {
  enqueueRetainedV2Frame(
    ws: ServerWebSocket<SyncWsData>,
    retained: SyncV2RetainedFrame,
    meta: SyncFeedFrameMeta,
  ): boolean;
  requestTerminalRebaseline(
    ws: ServerWebSocket<SyncWsData>,
    sessionId: string,
  ): boolean;
}

type TerminalCursorPump = "queued" | "blocked" | "empty";

export function makeSyncV2TerminalReadyScheduler(
  deps: SyncV2TerminalReadySchedulerDeps,
) {
  const { enqueueRetainedV2Frame, requestTerminalRebaseline } = deps;

  function markReady(
    v2: NonNullable<SyncWsData["v2"]>,
    sessionId: string,
    lane: SyncTerminalSessionLane,
  ): void {
    if (lane.ready) return;
    lane.ready = true;
    v2.terminalReadySessions.add(sessionId);
  }

  function takeReady(
    v2: NonNullable<SyncWsData["v2"]>,
    preferredSessionId?: string,
  ): [string, SyncTerminalSessionLane] | null {
    if (preferredSessionId !== undefined) {
      const lane = v2.terminalSessions.get(preferredSessionId);
      if (!lane?.ready) return null;
      lane.ready = false;
      v2.terminalReadySessions.delete(preferredSessionId);
      return [preferredSessionId, lane];
    }
    const next = v2.terminalReadySessions.values().next();
    if (next.done || next.value === undefined) return null;
    const sessionId = next.value;
    v2.terminalReadySessions.delete(sessionId);
    const lane = v2.terminalSessions.get(sessionId);
    if (!lane) return null;
    lane.ready = false;
    return [sessionId, lane];
  }

  function pumpState(
    ws: ServerWebSocket<SyncWsData>,
    sessionId: string,
    lane: SyncTerminalSessionLane,
  ): boolean {
    if (lane.stateQueued || lane.pendingStates.length === 0) return false;
    const queued = enqueueRetainedV2Frame(ws, lane.pendingStates[0]!, {
      domain: SyncDomain.TERMINAL,
      lane: "cell",
      sessionId,
      terminalStreamId: lane.streamId,
      terminalState: true,
    });
    if (queued) lane.stateQueued = true;
    return queued;
  }

  function pumpCursor(
    ws: ServerWebSocket<SyncWsData>,
    sessionId: string,
    lane: SyncTerminalSessionLane,
  ): TerminalCursorPump {
    const cursor = lane.cursor;
    if (!cursor || lane.streamId !== cursor.streamId) return "empty";
    if (cursor.queued) return "blocked";
    let retained: SyncV2RetainedFrame | undefined;
    let cursorIndex = cursor.index;
    if (cursor.index < cursor.frames.length) {
      retained = cursor.frames[cursor.index];
    } else if (cursor.deltaTail.length > 0) {
      retained = cursor.deltaTail[0];
      cursorIndex = cursor.frames.length;
    } else {
      lane.cursor = null;
      return "empty";
    }
    const attachSnapshot = lane.attachPriorityPending && isV2SnapshotFrame(retained.frame);
    if (!enqueueRetainedV2Frame(ws, retained, {
      domain: SyncDomain.TERMINAL,
      lane: "cell",
      sessionId,
      terminalStreamId: cursor.streamId,
      terminalCursorIndex: cursorIndex,
      attachSnapshot: attachSnapshot || undefined,
    })) return "blocked";
    cursor.queued = true;
    return "queued";
  }

  function pumpLane(
    ws: ServerWebSocket<SyncWsData>,
    sessionId: string,
    lane: SyncTerminalSessionLane,
  ): void {
    const v2 = ws.data.v2;
    if (!v2 || v2.terminalSessions.get(sessionId) !== lane) return;
    if (lane.stateQueued) return;
    if (lane.pendingStates.length > 0) {
      if (!pumpState(ws, sessionId, lane)) markReady(v2, sessionId, lane);
      return;
    }
    const cursorPump = pumpCursor(ws, sessionId, lane);
    if (cursorPump === "queued") return;
    if (cursorPump === "blocked") {
      markReady(v2, sessionId, lane);
      return;
    }
    if (lane.rebaselinePending && !requestTerminalRebaseline(ws, sessionId)) {
      markReady(v2, sessionId, lane);
    }
  }

  function pump(
    ws: ServerWebSocket<SyncWsData>,
    preferredSessionId?: string,
  ): void {
    const v2 = ws.data.v2;
    if (!v2) return;
    const ready = takeReady(v2, preferredSessionId);
    if (ready) pumpLane(ws, ready[0], ready[1]);
  }

  function onFrameDelivered(
    ws: ServerWebSocket<SyncWsData>,
    meta: SyncFeedFrameMeta,
  ): void {
    const sessionId = meta.sessionId;
    if (sessionId === undefined) return;
    const v2 = ws.data.v2;
    const lane = v2?.terminalSessions.get(sessionId);
    if (!v2 || !lane) return;
    const cursorIndex = meta.terminalCursorIndex;
    if (meta.terminalState) {
      if (!lane.stateQueued) return;
      lane.stateQueued = false;
      lane.pendingStates.shift();
    } else {
      if (cursorIndex === undefined) return;
      const cursor = lane.cursor;
      if (
        !cursor
        || cursor.streamId !== meta.terminalStreamId
        || cursorIndex !== (cursor.index < cursor.frames.length
          ? cursor.index
          : cursor.frames.length)
      ) return;
      cursor.queued = false;
      if (meta.attachSnapshot) lane.attachPriorityPending = false;
      if (cursor.index < cursor.frames.length) {
        cursor.index++;
      } else {
        const delta = cursor.deltaTail.shift();
        if (delta) cursor.deltaBytes -= delta.payloadBytes;
      }
    }
    markReady(v2, sessionId, lane);
  }

  return { markReady, pump, onFrameDelivered };
}
