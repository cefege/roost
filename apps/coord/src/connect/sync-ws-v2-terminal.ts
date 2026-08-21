// Per-session terminal stream queues layered over the sync v2 domain scheduler.

import type { ServerWebSocket } from "bun";
import { clone, toBinary } from "@bufbuild/protobuf";
import {
  FirehoseFrameSchema,
  SyncDomain,
  type FirehoseFrame,
} from "@roost/shared/proto/sync_pb";
import type { SyncFeedFrameMeta } from "./sync-feed.ts";
import type { SyncWsData } from "./sync-ws-handler.ts";
import {
  V2_DOMAIN_MAX_QUEUED_BYTES,
  V2_DOMAIN_MAX_QUEUED_FRAMES,
  type SyncTerminalSessionLane,
} from "./sync-ws-v2-state.ts";

interface SyncV2TerminalSchedulerDeps {
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
  const { enqueueV2Frame, removeTerminalQueued } = deps;

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
    cursor.queued = enqueueV2Frame(ws, frame, {
      domain: SyncDomain.TERMINAL,
      lane: "cell",
      sessionId,
      terminalStreamId: cursor.streamId,
      terminalCursorIndex: cursorIndex,
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
  ): void => {
    const v2 = ws.data.v2;
    if (!v2) return;
    const pendingStates =
      v2.terminalSessions.get(sessionId)?.pendingStates ?? [];
    removeTerminalQueued(ws, sessionId, false);
    const lane: SyncTerminalSessionLane = {
      streamId,
      cursor: null,
      pendingStates,
    };
    v2.terminalSessions.set(sessionId, lane);
    pumpTerminalStates(ws, sessionId, lane);
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
      lane = { streamId: "", cursor: null, pendingStates: [] };
      v2.terminalSessions.set(sessionId, lane);
    }
    lane.pendingStates.push(clone(FirehoseFrameSchema, frame));
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
    lane.cursor = {
      streamId,
      frames,
      index: 0,
      queued: false,
      deltaTail: [],
      deltaBytes: 0,
    };
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
