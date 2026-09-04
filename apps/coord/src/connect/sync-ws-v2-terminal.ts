// Per-session terminal stream queues layered over the sync v2 domain scheduler.
// The egress scheduler supplies aggregate retention and domain-reset boundaries.
// This owner preserves state, snapshot, and delta order across queue pressure.

import type { ServerWebSocket } from "bun";
import { toBinary } from "@bufbuild/protobuf";
import {
  FirehoseFrameSchema,
  SyncDomain,
  type FirehoseFrame,
} from "@roost/shared/proto/sync_pb";
import { log } from "@roost/shared/log";
import type { WsDeadlineClock } from "./ws-auth-deadline.ts";
import type { SyncFeedFrameMeta } from "./sync-feed.ts";
import type { SyncWsData } from "./sync-ws-handler.ts";
import {
  V2_ATTACH_PRIORITY_WINDOW_MS,
  V2_DOMAIN_MAX_QUEUED_BYTES,
  V2_DOMAIN_MAX_QUEUED_FRAMES,
  isV2SnapshotFrame,
  ownV2ApplicationFrame,
  releaseV2AggregateFrame,
  releaseV2TerminalCursor,
  releaseV2TerminalLane,
  tryRetainV2AggregateFrame,
  type SyncTerminalSessionLane,
  type SyncV2RetainedFrame,
} from "./sync-ws-v2-state.ts";

interface SyncV2TerminalSchedulerDeps {
  deadlineClock: WsDeadlineClock;
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
  onTerminalOverflow(ws: ServerWebSocket<SyncWsData>, reason: string): void;
}

export function makeSyncV2TerminalScheduler(
  deps: SyncV2TerminalSchedulerDeps,
) {
  const {
    deadlineClock,
    enqueueRetainedV2Frame,
    removeTerminalQueued,
    onTerminalOverflow,
  } = deps;
  // A backlogged socket re-enters this path on every state push until its
  // ACKs drain, so the overflow warning fires once per socket, not per frame.
  const overflowWarned = new WeakSet<ServerWebSocket<SyncWsData>>();
  function retainTerminalFrames(
    ws: ServerWebSocket<SyncWsData>,
    frames: readonly FirehoseFrame[],
  ): SyncV2RetainedFrame[] | null {
    const v2 = ws.data.v2;
    const terminal = v2?.domains.get(SyncDomain.TERMINAL);
    if (!v2 || !terminal) return null;
    const retained: SyncV2RetainedFrame[] = [];
    for (const frame of frames) {
      const owned = ownV2ApplicationFrame(frame, SyncDomain.TERMINAL, terminal.generation);
      const item = tryRetainV2AggregateFrame(v2, owned);
      if (item) {
        retained.push(item);
        continue;
      }
      for (const staged of retained) releaseV2AggregateFrame(v2, staged);
      return null;
    }
    return retained;
  }

  function resetForTerminalOverflow(
    ws: ServerWebSocket<SyncWsData>,
    sessionId: string,
    reason: string,
  ): void {
    if (!overflowWarned.has(ws)) {
      overflowWarned.add(ws);
      log.warn("sync-ws", reason, { session_id: sessionId });
    }
    onTerminalOverflow(ws, reason);
  }


  function pumpTerminalStates(
    ws: ServerWebSocket<SyncWsData>,
    sessionId: string,
    lane: SyncTerminalSessionLane,
  ): void {
    while (lane.pendingStates.length > 0) {
      const retained = lane.pendingStates[0]!;
      if (!enqueueRetainedV2Frame(ws, retained, {
        domain: SyncDomain.TERMINAL,
        lane: "cell",
        sessionId,
        terminalStreamId: lane.streamId,
      })) return;
      lane.pendingStates.shift();
    }
  }

  function pumpTerminalCursor(
    ws: ServerWebSocket<SyncWsData>,
    sessionId: string,
    lane: SyncTerminalSessionLane,
  ): void {
    const cursor = lane.cursor;
    if (!cursor || cursor.queued || lane.streamId !== cursor.streamId) return;
    if (lane.pendingStates.length > 0) return;
    let retained: SyncV2RetainedFrame | undefined;
    let cursorIndex = cursor.index;
    if (cursor.index < cursor.frames.length) {
      retained = cursor.frames[cursor.index];
    } else if (cursor.deltaTail.length > 0) {
      retained = cursor.deltaTail[0];
      cursorIndex = cursor.frames.length;
    } else {
      lane.cursor = null;
      return;
    }
    const startedAtMs = lane.snapshotStartedAtMs;
    // A fresh baseline jumps other sessions' queued deltas so a new viewer
    // paints ahead of steady-state traffic; the window keeps the privilege
    // from outliving the attach.
    const attachSnapshot = startedAtMs !== null
      && deadlineClock.now() - startedAtMs <= V2_ATTACH_PRIORITY_WINDOW_MS
      && isV2SnapshotFrame(retained.frame);
    cursor.queued = enqueueRetainedV2Frame(ws, retained, {
      domain: SyncDomain.TERMINAL,
      lane: "cell",
      sessionId,
      terminalStreamId: cursor.streamId,
      terminalCursorIndex: cursorIndex,
      attachSnapshot: attachSnapshot || undefined,
    });
  }

  const clearSessions = (ws: ServerWebSocket<SyncWsData>): void => {
    const v2 = ws.data.v2;
    if (!v2) return;
    for (const lane of v2.terminalSessions.values()) releaseV2TerminalLane(v2, lane);
    v2.terminalSessions.clear();
  };

  const pumpSessions = (ws: ServerWebSocket<SyncWsData>): void => {
    const v2 = ws.data.v2;
    if (!v2) return;
    for (const [sessionId, lane] of v2.terminalSessions) {
      pumpTerminalStates(ws, sessionId, lane);
      pumpTerminalCursor(ws, sessionId, lane);
    }
  };

  const deleteSession = (
    ws: ServerWebSocket<SyncWsData>,
    sessionId: string,
  ): void => {
    const v2 = ws.data.v2;
    const lane = v2?.terminalSessions.get(sessionId);
    if (!v2 || !lane) return;
    releaseV2TerminalLane(v2, lane);
    v2.terminalSessions.delete(sessionId);
  };

  const onFrameDelivered = (
    ws: ServerWebSocket<SyncWsData>,
    meta: SyncFeedFrameMeta,
  ): void => {
    const sessionId = meta.sessionId;
    const cursorIndex = meta.terminalCursorIndex;
    if (sessionId === undefined || cursorIndex === undefined) return;
    const lane = ws.data.v2?.terminalSessions.get(sessionId);
    const cursor = lane?.cursor;
    if (
      !lane
      || !cursor
      || cursor.streamId !== meta.terminalStreamId
      || cursorIndex !== (cursor.index < cursor.frames.length
        ? cursor.index
        : cursor.frames.length)
    ) return;
    cursor.queued = false;
    if (cursor.index < cursor.frames.length) {
      cursor.index++;
    } else {
      const delta = cursor.deltaTail.shift();
      if (delta) {
        cursor.deltaBytes -= delta.payloadBytes;
      }
    }
    pumpTerminalCursor(ws, sessionId, lane);
  };

  const beginTerminalStream = (
    ws: ServerWebSocket<SyncWsData>,
    sessionId: string,
    streamId: string,
  ): boolean => {
    const v2 = ws.data.v2;
    if (!v2) return false;
    const current = v2.terminalSessions.get(sessionId);
    if (current?.streamId === streamId) return false;
    const pendingStates = current?.pendingStates ?? [];
    removeTerminalQueued(ws, sessionId, false);
    if (current) releaseV2TerminalCursor(v2, current);
    const lane: SyncTerminalSessionLane = {
      streamId,
      cursor: null,
      pendingStates,
      snapshotStartedAtMs: deadlineClock.now(),
    };
    v2.terminalSessions.set(sessionId, lane);
    pumpTerminalStates(ws, sessionId, lane);
    return true;
  };

  const enqueueTerminalState = (
    ws: ServerWebSocket<SyncWsData>,
    frame: FirehoseFrame,
    sessionId: string,
  ): void => {
    const v2 = ws.data.v2;
    const terminal = v2?.domains.get(SyncDomain.TERMINAL);
    if (!v2 || !terminal) return;
    let lane = v2.terminalSessions.get(sessionId);
    if (!lane) {
      lane = { streamId: "", cursor: null, pendingStates: [], snapshotStartedAtMs: null };
      v2.terminalSessions.set(sessionId, lane);
    }
    if (lane.pendingStates.length + 1 > V2_DOMAIN_MAX_QUEUED_FRAMES) {
      resetForTerminalOverflow(ws, sessionId, "terminal_pending_states_overflow");
      return;
    }
    const retained = retainTerminalFrames(ws, [frame]);
    if (!retained) {
      resetForTerminalOverflow(ws, sessionId, "aggregate_overflow");
      return;
    }
    lane.pendingStates.push(retained[0]!);
    pumpTerminalStates(ws, sessionId, lane);
  };

  const replaceTerminalSnapshot = (
    ws: ServerWebSocket<SyncWsData>,
    sessionId: string,
    streamId: string,
    frames: readonly FirehoseFrame[],
  ): void => {
    const v2 = ws.data.v2;
    const lane = v2?.terminalSessions.get(sessionId);
    if (!v2 || !lane || lane.streamId !== streamId || frames.length === 0) return;
    removeTerminalQueued(ws, sessionId, false);
    releaseV2TerminalCursor(v2, lane);
    const retained = retainTerminalFrames(ws, frames);
    if (!retained) {
      resetForTerminalOverflow(ws, sessionId, "aggregate_overflow");
      return;
    }
    lane.cursor = {
      streamId,
      frames: retained,
      index: 0,
      queued: false,
      deltaTail: [],
      deltaBytes: 0,
    };
    lane.snapshotStartedAtMs = deadlineClock.now();
    pumpTerminalCursor(ws, sessionId, lane);
  };

  const enqueueTerminalDelta = (
    ws: ServerWebSocket<SyncWsData>,
    sessionId: string,
    streamId: string,
    frame: FirehoseFrame,
  ): boolean => {
    const v2 = ws.data.v2;
    const lane = v2?.terminalSessions.get(sessionId);
    if (!v2 || !lane || lane.streamId !== streamId) return false;
    const cursor = lane.cursor;
    if (!cursor) {
      const retained = retainTerminalFrames(ws, [frame]);
      if (!retained) {
        resetForTerminalOverflow(ws, sessionId, "aggregate_overflow");
        return false;
      }
      const item = retained[0]!;
      const queued = enqueueRetainedV2Frame(ws, item, {
        domain: SyncDomain.TERMINAL,
        lane: "cell",
        sessionId,
        terminalStreamId: streamId,
      });
      if (!queued) releaseV2AggregateFrame(v2, item);
      return queued;
    }
    const bytes = toBinary(FirehoseFrameSchema, frame).byteLength;
    if (
      cursor.deltaTail.length + 1 > V2_DOMAIN_MAX_QUEUED_FRAMES
      || cursor.deltaBytes + bytes > V2_DOMAIN_MAX_QUEUED_BYTES
    ) {
      resetForTerminalOverflow(ws, sessionId, "terminal_delta_tail_overflow");
      return false;
    }
    const retained = retainTerminalFrames(ws, [frame]);
    if (!retained) {
      resetForTerminalOverflow(ws, sessionId, "aggregate_overflow");
      return false;
    }
    cursor.deltaTail.push({ ...retained[0]!, payloadBytes: bytes });
    cursor.deltaBytes += bytes;
    return true;
  };

  const dropTerminalSession = (
    ws: ServerWebSocket<SyncWsData>,
    sessionId: string,
  ): void => {
    const v2 = ws.data.v2;
    if (!v2) return;
    removeTerminalQueued(ws, sessionId, false);
    const lane = v2.terminalSessions.get(sessionId);
    if (lane) {
      releaseV2TerminalCursor(v2, lane);
      lane.streamId = "";
      pumpTerminalStates(ws, sessionId, lane);
    }
  };

  return {
    clearSessions,
    pumpSessions,
    deleteSession,
    onFrameDelivered,
    beginTerminalStream,
    enqueueTerminalState,
    replaceTerminalSnapshot,
    enqueueTerminalDelta,
    dropTerminalSession,
  };
}
