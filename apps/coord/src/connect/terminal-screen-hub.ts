// Per-session terminal screen cache and fan-out: assembles chunked baselines,
// folds deltas onto the canonical cache, seeds new watchers, and latches a
// resync whenever anything fails validation instead of serving wrong pixels.
// residentRows/residentSpans are global counters that MUST be adjusted on every
// cache install/drop — drift silently exhausts capacity for all sessions.
// Deltas arriving mid-assembly park in the bounded hold; an interrupting
// non-matching frame clears it and requests a fresh snapshot.
import { clone } from "@bufbuild/protobuf";
import {
  applyDelta, assertCellGridSnapshot, CellGridChunkAssembler, CELL_GRID_CHUNK_STALL_MS,
  CELL_GRID_PART_MAX_BYTES, CELL_GRID_SNAPSHOT_MAX_SPANS, cloneCellGridFrame,
  encodedCellGridFrameSize, type CellGridFrame,
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
  TerminalAssemblyHold,
  type ChunkState,
  type ExpectedStream,
  type ResidentCache,
  type SessionScreen,
  type SocketRegistration,
} from "./terminal-screen-hub-state.ts";

export { terminalSnapshotFrames };

export const TERMINAL_SCREEN_MAX_RESIDENT_ROWS = 65_536;
export const TERMINAL_SCREEN_MAX_RESIDENT_SPANS = 2_097_152;

export interface TerminalScreenSocketSink {
  beginTerminalStream(sessionId: string, streamId: string): boolean;
  enqueueTerminalState(frame: FirehoseFrame, sessionId: string): void;
  replaceTerminalSnapshot(
    sessionId: string,
    streamId: string,
    frames: readonly FirehoseFrame[],
  ): void;
  enqueueTerminalDelta(sessionId: string, streamId: string, frame: FirehoseFrame): boolean;
  dropTerminalSession(sessionId: string): void;
}

export interface TerminalScreenHubOptions {
  requestSnapshot(sessionId: string, streamId: string): void;
  unavailable?(sessionId: string, reason: string): void;
  setTimer?: (callback: () => void, delayMs: number) => ReturnType<typeof setTimeout>;
  clearTimer?: (timer: ReturnType<typeof setTimeout>) => void;
  now?: () => number;
}

export class TerminalScreenHub {
  private readonly sessions = new Map<string, SessionScreen>();
  private readonly sockets = new Map<string, SocketRegistration>();
  private readonly requestSnapshot: TerminalScreenHubOptions["requestSnapshot"];
  private readonly unavailable: NonNullable<TerminalScreenHubOptions["unavailable"]>;
  private readonly setTimer: NonNullable<TerminalScreenHubOptions["setTimer"]>;
  private readonly clearTimer: NonNullable<TerminalScreenHubOptions["clearTimer"]>;
  private readonly now: () => number;
  private residentRows = 0;
  private residentSpans = 0;

  constructor(options: TerminalScreenHubOptions) {
    this.requestSnapshot = options.requestSnapshot;
    this.unavailable = options.unavailable ?? (() => undefined);
    this.setTimer = options.setTimer ?? ((callback, delayMs) => setTimeout(callback, delayMs));
    this.clearTimer = options.clearTimer ?? clearTimeout;
    this.now = options.now ?? Date.now;
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
    this.seed(socket, sessionId, state.expected.streamId, state.cache.proto);
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
      this.seed(socket, sessionId, state.expected.streamId, state.cache.proto);
      return;
    }
    this.retryResync(sessionId, state, "browser requested terminal rebaseline");
  }

  expectStream(sessionId: string, streamId: string, cols: number, rows: number): void {
    const state = this.getSession(sessionId);
    if (state.expected?.streamId === streamId
      && state.expected.cols === cols
      && state.expected.rows === rows) return;
    this.resetChunks(state);
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
      this.resetChunks(state);
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
    this.resetChunks(state);
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
    this.resetChunks(state);
    state.hold.clear();
    if (state.cache) state.cache.valid = false;
    this.latchResync(sessionId, state, reason);
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
      this.resetChunks(state);
      if (interrupts) this.latchResync(sessionId, state, "ordinary frame interrupted chunk assembly");
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
      const result = state.chunks.assembler.push(chunk, this.now());
      this.armChunkTimer(sessionId, state);
      if (result.kind === "pending") return;
      this.resetChunks(state);
      this.acceptFull(sessionId, state, result.frame, true);
    } catch (error) {
      state.hold.clear();
      this.resetChunks(state);
      this.latchResync(
        sessionId,
        state,
        error instanceof Error ? error.message : "invalid terminal snapshot chunk",
      );
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
      this.installCache(sessionId, state, frame, canonical, stats.spans);
    } catch (error) {
      this.latchResync(
        sessionId,
        state,
        error instanceof Error ? error.message : "invalid terminal baseline",
      );
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
      this.latchResync(sessionId, state, "terminal delta arrived before a complete baseline");
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
        if (!socket.sink.enqueueTerminalDelta(sessionId, expected.streamId, outbound)) {
          this.seed(socket, sessionId, expected.streamId, canonical);
        }
      }
    } catch (error) {
      cache.valid = false;
      this.latchResync(
        sessionId,
        state,
        error instanceof Error ? error.message : "invalid terminal delta",
      );
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
      if (socket.watchedSessions.has(sessionId)) this.seed(socket, sessionId, streamId, proto);
    }
  }

  private seed(
    socket: SocketRegistration,
    sessionId: string,
    streamId: string,
    proto: PbCellGridFrame,
  ): void {
    try {
      socket.sink.replaceTerminalSnapshot(
        sessionId,
        streamId,
        terminalSnapshotFrames(clone(PbCellGridFrameSchema, proto)),
      );
    } catch (error) {
      const reason = error instanceof Error ? error.message : "terminal snapshot encoding failed";
      this.unavailable(sessionId, reason);
      signal("terminal.snapshot_encode_failed", { session_id: sessionId, reason });
    }
  }

  private getSession(sessionId: string): SessionScreen {
    let state = this.sessions.get(sessionId);
    if (!state) {
      state = {
        expected: null,
        cache: null,
        chunks: { assembler: new CellGridChunkAssembler(), timer: null },
        resyncLatched: false,
        hold: new TerminalAssemblyHold(),
      };
      this.sessions.set(sessionId, state);
    }
    return state;
  }

  private latchResync(sessionId: string, state: SessionScreen, reason: string): void {
    this.requestResync(sessionId, state, reason, false);
  }

  private retryResync(sessionId: string, state: SessionScreen, reason: string): void {
    if (state.chunks.assembler.activeSnapshotId !== null) return;
    this.requestResync(sessionId, state, reason, true);
  }

  private requestResync(
    sessionId: string,
    state: SessionScreen,
    reason: string,
    retry: boolean,
  ): void {
    if (!state.expected || (!retry && state.resyncLatched)) return;
    state.resyncLatched = true;
    diag("terminal.screen_resync", {
      session_id: sessionId,
      stream_id: state.expected.streamId,
      reason,
    });
    this.requestSnapshot(sessionId, state.expected.streamId);
  }

  private armChunkTimer(sessionId: string, state: SessionScreen): void {
    if (state.chunks.timer) this.clearTimer(state.chunks.timer);
    state.chunks.timer = this.setTimer(() => {
      state.chunks.timer = null;
      if (!state.chunks.assembler.expire(this.now())) return;
      state.hold.clear();
      this.retryResync(sessionId, state, "terminal snapshot chunk transfer stalled");
    }, CELL_GRID_CHUNK_STALL_MS);
    state.chunks.timer.unref?.();
  }

  private resetChunks(state: SessionScreen): void {
    if (state.chunks.timer) this.clearTimer(state.chunks.timer);
    state.chunks.timer = null;
    state.chunks.assembler.reset();
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
