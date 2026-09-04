// Owns the canonical per-session terminal screen cache and watcher fan-out.
// Invalid baselines or deltas fail closed and request repair instead of serving
// wrong pixels. Resident row/span counters change with every cache install or
// drop so one session cannot silently exhaust capacity for the entire process.

import { clone } from "@bufbuild/protobuf";
import {
  applyDelta, assertCellGridSnapshot, CELL_GRID_PART_MAX_BYTES,
  CELL_GRID_SNAPSHOT_MAX_SPANS, cloneCellGridFrame, encodedCellGridFrameSize,
  type CellGridFrame,
} from "@roost/shared/cell";
import { cellFrameToProto, protoToCellFrame } from "@roost/shared/cell/cell-proto";
import {
  PbCellGridFrameSchema,
  type PbCellGridChunk,
  type PbCellGridFrame,
} from "@roost/shared/proto/cell_pb";
import type { FirehoseFrame } from "@roost/shared/proto/sync_pb";
import { diag, signal } from "@roost/shared/diag";
import {
  cellGridEnvelope,
  countCellGridSpans,
  countCellGridRows,
  normalizeCellGridFrame,
  terminalScreenSnapshot,
  terminalSnapshotFrames,
  type TerminalScreenSnapshot,
} from "./terminal-screen-frames.ts";
import {
  type SessionScreen,
  type SocketRegistration,
} from "./terminal-screen-hub-state.ts";
import { TerminalScreenSnapshotController } from "./terminal-screen-snapshot-controller.ts";

export { terminalSnapshotFrames };

export const TERMINAL_SCREEN_MAX_RESIDENT_ROWS = 65_536;
export const TERMINAL_SCREEN_MAX_RESIDENT_SPANS = 2_097_152;
export { TERMINAL_SNAPSHOT_FIRST_BYTE_TIMEOUT_MS } from "./terminal-screen-snapshot-controller.ts";

export type TerminalDeltaEnqueueResult = "queued" | "needs_snapshot" | "handled";

export interface TerminalScreenSocketSink {
  beginTerminalStream(sessionId: string, streamId: string): boolean;
  enqueueTerminalState(frame: FirehoseFrame, sessionId: string): void;
  replaceTerminalSnapshot(
    sessionId: string,
    streamId: string,
    frames: readonly FirehoseFrame[],
  ): void;
  enqueueTerminalDelta(
    sessionId: string,
    streamId: string,
    frame: FirehoseFrame,
  ): TerminalDeltaEnqueueResult;
  dropTerminalSession(sessionId: string): void;
}

export interface TerminalScreenHubOptions {
  requestSnapshot(sessionId: string, streamId: string): void;
  unavailable?(sessionId: string, reason: string): void;
  requestFreshStream(sessionId: string, expectedStreamId: string, reason: string): void;
  setTimer?: (callback: () => void, delayMs: number) => ReturnType<typeof setTimeout>;
  clearTimer?: (timer: ReturnType<typeof setTimeout>) => void;
  now?: () => number;
}

export class TerminalScreenHub {
  private readonly sessions = new Map<string, SessionScreen>();
  private readonly sockets = new Map<string, SocketRegistration>();
  private readonly unavailable: NonNullable<TerminalScreenHubOptions["unavailable"]>;
  private readonly now: () => number;
  private readonly snapshots: TerminalScreenSnapshotController;
  private residentRows = 0;
  private residentSpans = 0;

  constructor(options: TerminalScreenHubOptions) {
    this.unavailable = options.unavailable ?? (() => undefined);
    this.now = options.now ?? Date.now;
    this.snapshots = new TerminalScreenSnapshotController({
      sessions: this.sessions,
      requestSnapshot: options.requestSnapshot,
      unavailable: this.unavailable,
      requestFreshStream: options.requestFreshStream,
      setTimer: options.setTimer ?? ((callback, delayMs) => setTimeout(callback, delayMs)),
      clearTimer: options.clearTimer ?? clearTimeout,
      now: this.now,
    });
  }

  dispose(): void {
    for (const state of this.sessions.values()) {
      this.snapshots.reset(state, true);
      this.dropCache(state);
    }
    this.sessions.clear();
    for (const socketId of [...this.sockets.keys()]) this.unregisterSocket(socketId);
  }

  registerSocket(socketId: string, sink: TerminalScreenSocketSink): void {
    this.unregisterSocket(socketId);
    this.sockets.set(socketId, { sink, watchedSessions: new Set() });
  }

  unregisterSocket(socketId: string): void {
    const socket = this.sockets.get(socketId);
    if (!socket) return;
    for (const sessionId of socket.watchedSessions) socket.sink.dropTerminalSession(sessionId);
    this.sockets.delete(socketId);
  }

  setWatching(socketId: string, sessionId: string, watching: boolean): void {
    const socket = this.sockets.get(socketId);
    if (!socket) return;
    if (!watching) {
      if (socket.watchedSessions.delete(sessionId)) socket.sink.dropTerminalSession(sessionId);
      return;
    }
    if (socket.watchedSessions.has(sessionId)) return;
    socket.watchedSessions.add(sessionId);
    const state = this.sessions.get(sessionId);
    if (!state?.expected) return;
    socket.sink.beginTerminalStream(sessionId, state.expected.streamId);
  }

  seedSocket(socketId: string, sessionId: string): void {
    const socket = this.sockets.get(socketId);
    const state = this.sessions.get(sessionId);
    if (!socket?.watchedSessions.has(sessionId) || !state?.expected || !state.cache?.valid) return;
    this.snapshots.seed(socket, sessionId, state.expected.streamId, state.cache.proto);
  }

  ensureSocketStream(socketId: string, sessionId: string): boolean {
    const socket = this.sockets.get(socketId);
    const state = this.sessions.get(sessionId);
    if (!socket?.watchedSessions.has(sessionId) || !state?.expected) return false;
    return socket.sink.beginTerminalStream(sessionId, state.expected.streamId);
  }

  resyncSocket(socketId: string, sessionId: string): void {
    const socket = this.sockets.get(socketId);
    const state = this.sessions.get(sessionId);
    if (!socket?.watchedSessions.has(sessionId) || !state?.expected) return;
    socket.sink.beginTerminalStream(sessionId, state.expected.streamId);
    if (state.cache?.valid && !state.resyncLatched) {
      this.snapshots.seed(socket, sessionId, state.expected.streamId, state.cache.proto);
      return;
    }
    this.snapshots.retry(sessionId, state, "browser requested terminal rebaseline");
  }

  expectStream(sessionId: string, streamId: string, cols: number, rows: number): void {
    const state = this.snapshots.getSession(sessionId);
    if (state.expected?.streamId === streamId
      && state.expected.cols === cols
      && state.expected.rows === rows) return;
    this.snapshots.reset(state, true);
    this.dropCache(state);
    state.hold.clear();
    state.expected = { streamId, cols, rows };
    state.resyncLatched = false;
    for (const socket of this.sockets.values()) {
      if (socket.watchedSessions.has(sessionId)) {
        socket.sink.beginTerminalStream(sessionId, streamId);
      }
    }
  }

  dropSession(sessionId: string): void {
    const state = this.sessions.get(sessionId);
    if (state) {
      this.snapshots.reset(state, true);
      this.dropCache(state);
      this.sessions.delete(sessionId);
    }
    for (const socket of this.sockets.values()) {
      if (socket.watchedSessions.delete(sessionId)) socket.sink.dropTerminalSession(sessionId);
    }
  }

  failClosed(sessionId: string, reason: string): void {
    const state = this.sessions.get(sessionId);
    if (!state?.expected) return;
    this.snapshots.resetChunks(state);
    state.hold.clear();
    if (state.cache) state.cache.valid = false;
    state.resyncLatched = true;
    diag("terminal.screen_fail_closed", {
      session_id: sessionId,
      stream_id: state.expected.streamId,
      reason,
    });
  }

  invalidate(sessionId: string, reason: string): void {
    const state = this.sessions.get(sessionId);
    if (!state?.expected) return;
    this.snapshots.resetChunks(state);
    state.hold.clear();
    if (state.cache) state.cache.valid = false;
    this.snapshots.retry(sessionId, state, reason);
  }

  publishFrame(sessionId: string, frame: PbCellGridFrame): void {
    const state = this.sessions.get(sessionId);
    if (!state?.expected) return;
    frame.sessionId = sessionId;
    if (state.chunks.assembler.activeSnapshotId !== null) {
      // Deltas park in the bounded hold during assembly; a matching full supersedes; else resync.
      const interrupts = !frame.full || frame.streamId !== state.expected.streamId;
      if (!frame.full && state.hold.push(frame)) return;
      state.hold.clear();
      this.snapshots.resetChunks(state);
      if (interrupts) {
        this.snapshots.retry(sessionId, state, "ordinary frame interrupted chunk assembly");
      }
      if (frame.full) this.acceptFull(sessionId, state, frame, false);
      return;
    }
    if (frame.full) this.acceptFull(sessionId, state, frame, false);
    else this.acceptDelta(sessionId, state, frame);
  }

  publishChunk(sessionId: string, chunk: PbCellGridChunk): void {
    const state = this.sessions.get(sessionId);
    if (!state?.expected) return;
    if (chunk.part?.streamId !== state.expected.streamId) return;
    if (chunk.part) chunk.part.sessionId = sessionId;
    try {
      const firstChunk = state.chunks.assembler.activeSnapshotId === null;
      const result = state.chunks.assembler.push(chunk, this.now());
      if (firstChunk) this.snapshots.cancelRequestTimer(state, true);
      this.snapshots.armChunkTimer(sessionId, state);
      if (result.kind === "pending") return;
      this.snapshots.resetChunks(state);
      this.acceptFull(sessionId, state, result.frame, true);
    } catch (error) {
      state.hold.clear();
      this.snapshots.resetChunks(state);
      this.snapshots.retry(sessionId,
      state,
      error instanceof Error ? error.message : "invalid terminal snapshot chunk",);
    }
  }

  private acceptFull(
    sessionId: string,
    state: SessionScreen,
    proto: PbCellGridFrame,
    assembled: boolean,
  ): void {
    const expected = state.expected!;
    if (proto.streamId !== expected.streamId) return;
    try {
      if (!assembled && encodedCellGridFrameSize(proto) > CELL_GRID_PART_MAX_BYTES) {
        throw new Error("unchunked terminal full exceeds part limit");
      }
      const stats = assertCellGridSnapshot(proto);
      if (proto.cols !== expected.cols || proto.rows !== expected.rows) {
        throw new Error("terminal baseline geometry does not match expected stream");
      }
      const frame = cloneCellGridFrame(normalizeCellGridFrame(protoToCellFrame(proto)));
      const canonical = cellFrameToProto(frame, sessionId);
      canonical.streamId = expected.streamId;
      canonical.baseSeq = 0n;
      this.snapshots.complete(state);
      this.installCache(sessionId, state, frame, canonical, stats.spans);
    } catch (error) {
      this.snapshots.retry(sessionId,
      state,
      error instanceof Error ? error.message : "invalid terminal baseline",);
    }
    state.hold.replay(
      () => (state.cache ? BigInt(state.cache.frame.seq) : null),
      () => Boolean(state.cache?.valid) && !state.resyncLatched,
      (delta) => this.acceptDelta(sessionId, state, delta),
    );
  }

  private acceptDelta(sessionId: string, state: SessionScreen, proto: PbCellGridFrame): void {
    const expected = state.expected!;
    if (proto.streamId !== expected.streamId) return;
    const cache = state.cache;
    if (!cache?.valid) {
      this.snapshots.latch(sessionId, state, "terminal delta arrived before a complete baseline");
      return;
    }
    try {
      if (proto.full
        || proto.baseSeq !== BigInt(cache.frame.seq)
        || proto.seq !== proto.baseSeq + 1n
        || proto.gridEpoch !== cache.frame.gridEpoch
        || proto.cols !== expected.cols
        || proto.rows !== expected.rows) {
        throw new Error("terminal delta does not follow the canonical baseline");
      }
      if (encodedCellGridFrameSize(proto) > CELL_GRID_PART_MAX_BYTES) {
        throw new Error("terminal delta exceeds part limit");
      }
      const folded = applyDelta(cloneCellGridFrame(cache.frame), protoToCellFrame(proto));
      if (!folded) throw new Error("terminal delta cannot be folded into baseline");
      normalizeCellGridFrame(folded);
      const spans = countCellGridSpans(folded);
      const rows = countCellGridRows(folded);
      if (spans > CELL_GRID_SNAPSHOT_MAX_SPANS) throw new Error("terminal cache span limit exceeded");
      if (this.residentRows - cache.rows + rows > TERMINAL_SCREEN_MAX_RESIDENT_ROWS
        || this.residentSpans - cache.spans + spans > TERMINAL_SCREEN_MAX_RESIDENT_SPANS) {
        throw new Error("coordinator terminal cache capacity exceeded");
      }
      const canonical = cellFrameToProto(folded, sessionId);
      canonical.streamId = expected.streamId;
      canonical.baseSeq = 0n;
      this.residentRows += rows - cache.rows;
      this.residentSpans += spans - cache.spans;
      state.cache = { frame: folded, proto: canonical, rows, spans, valid: true };
      state.resyncLatched = false;
      const outbound = cellGridEnvelope(clone(PbCellGridFrameSchema, proto));
      for (const socket of this.sockets.values()) {
        if (!socket.watchedSessions.has(sessionId)) continue;
        const result = socket.sink.enqueueTerminalDelta(
          sessionId,
          expected.streamId,
          outbound,
        );
        if (result === "needs_snapshot") {
          this.snapshots.seed(socket, sessionId, expected.streamId, canonical);
        }
      }
    } catch (error) {
      cache.valid = false;
      this.snapshots.latch(sessionId,
      state,
      error instanceof Error ? error.message : "invalid terminal delta",);
    }
  }

  private installCache(
    sessionId: string,
    state: SessionScreen,
    frame: CellGridFrame,
    proto: PbCellGridFrame,
    spans: number,
  ): void {
    const oldRows = state.cache?.rows ?? 0;
    const oldSpans = state.cache?.spans ?? 0;
    const rows = countCellGridRows(frame);
    if (this.residentRows - oldRows + rows > TERMINAL_SCREEN_MAX_RESIDENT_ROWS
      || this.residentSpans - oldSpans + spans > TERMINAL_SCREEN_MAX_RESIDENT_SPANS) {
      this.dropCache(state);
      const reason = "coordinator terminal cache capacity exceeded";
      this.unavailable(sessionId, reason);
      signal("terminal.screen_capacity", { session_id: sessionId, rows, spans });
      return;
    }
    this.residentRows += rows - oldRows;
    this.residentSpans += spans - oldSpans;
    state.cache = { frame, proto, rows, spans, valid: true };
    state.resyncLatched = false;
    const streamId = state.expected!.streamId;
    for (const socket of this.sockets.values()) {
      if (socket.watchedSessions.has(sessionId)) this.snapshots.seed(socket, sessionId, streamId, proto);
    }
  }

  private dropCache(state: SessionScreen): void {
    if (!state.cache) return;
    this.residentRows -= state.cache.rows;
    this.residentSpans -= state.cache.spans;
    state.cache = null;
  }

  snapshot(sessionId: string): TerminalScreenSnapshot | null {
    const state = this.sessions.get(sessionId);
    return terminalScreenSnapshot(state?.expected, state?.cache);
  }
}
