// Per-session terminal stream queues layered over the sync v2 domain scheduler.

import type { ServerWebSocket } from "bun";
import { clone, toBinary } from "@bufbuild/protobuf";
import {
  FirehoseFrameSchema,
  SyncDomain,
  type FirehoseFrame,
} from "@roost/shared/proto/sync_pb";
import { log } from "@roost/shared/log";
import type { SyncDeadlineClock } from "./sync-ws-deadline.ts";
import type { SyncFeedFrameMeta } from "./sync-feed.ts";
import type { SyncWsData } from "./sync-ws-handler.ts";
import {
  V2_ATTACH_PRIORITY_WINDOW_MS,
  V2_DOMAIN_MAX_QUEUED_BYTES,
  V2_DOMAIN_MAX_QUEUED_FRAMES,
  isV2SnapshotFrame,
  type SyncTerminalSessionLane,
} from "./sync-ws-v2-state.ts";

interface SyncV2TerminalSchedulerDeps {
  deadlineClock: SyncDeadlineClock;
  enqueueV2Frame(
    ws: ServerWebSocket<SyncWsData>,
    frame: FirehoseFrame,
    meta?: SyncFeedFrameMeta,
  ): boolean;
  removeTerminalQueued(
    ws: ServerWebSocket<SyncWsData>,
    sessionId: string,
    includeState: boolean,
  ): void;
}

export function makeSyncV2TerminalScheduler(
  deps: SyncV2TerminalSchedulerDeps,
) {
  const { deadlineClock, enqueueV2Frame, removeTerminalQueued } = deps;
  // A backlogged socket re-enters this path on every state push until its
  // ACKs drain, so the overflow warning fires once per socket, not per frame.
  const overflowWarned = new WeakSet<ServerWebSocket<SyncWsData>>();

  function pumpTerminalStates(
    ws: ServerWebSocket<SyncWsData>,
    sessionId: string,
    lane: SyncTerminalSessionLane,
  ): void {
    while (lane.pendingStates.length > 0) {
      const frame = lane.pendingStates[0]!;
      if (!enqueueV2Frame(ws, frame, {
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
    let frame: FirehoseFrame | undefined;
    let cursorIndex = cursor.index;
    if (cursor.index < cursor.frames.length) {
      frame = cursor.frames[cursor.index];
    } else if (cursor.deltaTail.length > 0) {
      frame = cursor.deltaTail[0];
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
      && isV2SnapshotFrame(frame);
    cursor.queued = enqueueV2Frame(ws, frame, {
      domain: SyncDomain.TERMINAL,
      lane: "cell",
      sessionId,
      terminalStreamId: cursor.streamId,
      terminalCursorIndex: cursorIndex,
      attachSnapshot: attachSnapshot || undefined,
    });
  }

  const clearSessions = (ws: ServerWebSocket<SyncWsData>): void => {
    ws.data.v2?.terminalSessions.clear();
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
    ws.data.v2?.terminalSessions.delete(sessionId);
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
        cursor.deltaBytes -= toBinary(FirehoseFrameSchema, delta).byteLength;
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
    if (!v2) return;
    let lane = v2.terminalSessions.get(sessionId);
    if (!lane) {
      lane = { streamId: "", cursor: null, pendingStates: [], snapshotStartedAtMs: null };
      v2.terminalSessions.set(sessionId, lane);
    }
    lane.pendingStates.push(clone(FirehoseFrameSchema, frame));
    pumpTerminalStates(ws, sessionId, lane);
    if (lane.pendingStates.length > V2_DOMAIN_MAX_QUEUED_FRAMES) {
      // Same frame cap the domain queue enforces. Shed the OLDEST states:
      // a newer terminalViewState supersedes older ones, so keeping the
      // newest tail is what leaves a correct view once the queue drains
      // (dropping newest would strand a stale view instead).
      lane.pendingStates.splice(
        0,
        lane.pendingStates.length - V2_DOMAIN_MAX_QUEUED_FRAMES,
      );
      if (!overflowWarned.has(ws)) {
        overflowWarned.add(ws);
        log.warn("sync-ws", "terminal_pending_states_overflow", {
          session_id: sessionId,
          cap: V2_DOMAIN_MAX_QUEUED_FRAMES,
        });
      }
    }
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
    lane.cursor = {
      streamId,
      frames,
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
      return enqueueV2Frame(ws, frame, {
        domain: SyncDomain.TERMINAL,
        lane: "cell",
        sessionId,
        terminalStreamId: streamId,
      });
    }
    const bytes = toBinary(FirehoseFrameSchema, frame).byteLength;
    if (
      cursor.deltaTail.length + 1 > V2_DOMAIN_MAX_QUEUED_FRAMES
      || cursor.deltaBytes + bytes > V2_DOMAIN_MAX_QUEUED_BYTES
    ) return false;
    cursor.deltaTail.push(clone(FirehoseFrameSchema, frame));
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
      lane.streamId = "";
      lane.cursor = null;
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
