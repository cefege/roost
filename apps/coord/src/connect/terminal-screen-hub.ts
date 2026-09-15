// Owns current canonical terminal screens and source-pinned predecessor versions.
// Invalid baselines or deltas fail closed and request repair instead of serving wrong pixels.
// Active snapshot cursors keep their immutable source versions resident.

import { clone } from "@bufbuild/protobuf";
import {
  applyDelta, assertCellGridSnapshot, CELL_GRID_COORD_FANOUT_STAMP_MAX_ENCODED_BYTES,
  CELL_GRID_PART_MAX_BYTES, CELL_GRID_SNAPSHOT_MAX_SPANS, cloneCellGridFrame,
  encodedCellGridFrameSize, normalizeCellGridFrame,
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
  countTerminalScreenCacheSpans,
  terminalScreenSnapshot,
  terminalSnapshotSource,
  type TerminalScreenSnapshot,
  type TerminalSnapshotSource,
} from "./terminal-screen-frames.ts";
import {
  attachTerminalScreenWatcher,
  detachTerminalScreenSocket,
  detachTerminalScreenSockets,
  detachTerminalScreenWatcher,
  detachTerminalScreenWatchers, stampTerminalSnapshotReceipt,
  type SessionScreen,
  type SocketRegistration,
} from "./terminal-screen-hub-state.ts";
import { TerminalScreenResidency } from "./terminal-screen-residency.ts";
import { recordCoordinatorFrame } from "./terminal-capture-recorder.ts";
import { TerminalScreenSnapshotController } from "./terminal-screen-snapshot-controller.ts";
import type {
  TerminalScreenHubOptions,
  TerminalScreenSocketSink,
} from "./terminal-screen-hub-contract.ts";

export const TERMINAL_SCREEN_MAX_RESIDENT_ROWS = 65_536;
export const TERMINAL_SCREEN_MAX_RESIDENT_SPANS = 2_097_152;
export { TERMINAL_SNAPSHOT_FIRST_BYTE_TIMEOUT_MS } from "./terminal-screen-snapshot-controller.ts";
export type {
  TerminalDeltaEnqueueResult,
  TerminalScreenHubOptions,
  TerminalScreenSocketSink,
} from "./terminal-screen-hub-contract.ts";

export class TerminalScreenHub {
  private readonly sessions = new Map<string, SessionScreen>();
  private readonly sockets = new Map<string, SocketRegistration>();
  private readonly watchersBySession = new Map<string, Set<string>>();
  private readonly unavailable: NonNullable<TerminalScreenHubOptions["unavailable"]>;
  private readonly now: () => number;
  private readonly snapshots: TerminalScreenSnapshotController;
  private readonly residency = new TerminalScreenResidency(
    TERMINAL_SCREEN_MAX_RESIDENT_ROWS,
    TERMINAL_SCREEN_MAX_RESIDENT_SPANS,
  );

  constructor(private readonly options: TerminalScreenHubOptions) {
    this.unavailable = options.unavailable ?? (() => undefined);
    this.now = options.now ?? Date.now;
    this.snapshots = new TerminalScreenSnapshotController({
      sessions: this.sessions,
      requestSnapshot: options.requestSnapshot,
      unavailable: this.unavailable,
      requestFreshStream: options.requestFreshStream,
      snapshotSource: (sessionId, cache) => {
        const frame = cache.frame;
        return terminalSnapshotSource(
          () => Object.assign(cellFrameToProto(frame, sessionId), { coordRecvMs: cache.coordRecvMs }),
          this.residency.sourceLease(cache),
        );
      },
      setTimer: options.setTimer ?? ((callback, delayMs) => setTimeout(callback, delayMs)),
      clearTimer: options.clearTimer ?? clearTimeout,
      now: this.now,
    });
  }

  dispose(): void {
    const detached = detachTerminalScreenSockets(this.watchersBySession, this.sockets);
    for (const state of this.sessions.values()) {
      this.snapshots.reset(state, true);
      this.dropCache(state);
    }
    this.sessions.clear();
    for (const [socket, sessionId] of detached) socket.sink.dropTerminalSession(sessionId);
    detachTerminalScreenSockets(this.watchersBySession, this.sockets);
  }

  registerSocket(socketId: string, sink: TerminalScreenSocketSink): void {
    this.unregisterSocket(socketId);
    // A retirement callback can install a newer registration that must not be overwritten.
    if (this.sockets.has(socketId)) return;
    this.sockets.set(socketId, { sink, watchedSessions: new Set() });
  }

  unregisterSocket(socketId: string): void {
    const socket = this.sockets.get(socketId);
    if (!socket) return;
    const watchedSessionIds = detachTerminalScreenSocket(this.watchersBySession, this.sockets, socketId, socket);
    for (const sessionId of watchedSessionIds) socket.sink.dropTerminalSession(sessionId);
  }

  setWatching(socketId: string, sessionId: string, watching: boolean): void {
    const socket = this.sockets.get(socketId);
    if (!socket) return;
    if (!watching) {
      if (detachTerminalScreenWatcher(this.watchersBySession, socketId, socket, sessionId)) socket.sink.dropTerminalSession(sessionId);
      return;
    }
    if (!attachTerminalScreenWatcher(this.watchersBySession, socketId, socket, sessionId)) return;
    const state = this.sessions.get(sessionId);
    if (!state?.expected) return;
    socket.sink.beginTerminalStream(sessionId, state.expected.streamId);
  }

  seedSocket(socketId: string, sessionId: string): boolean {
    const socket = this.sockets.get(socketId);
    const state = this.sessions.get(sessionId);
    if (!socket?.watchedSessions.has(sessionId) || !state?.expected || !state.cache?.valid) {
      return false;
    }
    return this.snapshots.seed(socket, sessionId, state.expected.streamId, state.cache);
  }

  ensureSocketStream(socketId: string, sessionId: string): boolean {
    const socket = this.sockets.get(socketId);
    const state = this.sessions.get(sessionId);
    if (!socket?.watchedSessions.has(sessionId) || !state?.expected) return false;
    return socket.sink.beginTerminalStream(sessionId, state.expected.streamId);
  }

  resyncSocket(
    socketId: string,
    sessionId: string,
    checkpoint: Readonly<{ gridEpoch: string; seq: bigint }> | null,
  ): boolean {
    const socket = this.sockets.get(socketId);
    const state = this.sessions.get(sessionId);
    if (!socket?.watchedSessions.has(sessionId) || !state?.expected) return false;
    socket.sink.beginTerminalStream(sessionId, state.expected.streamId);
    const cache = state.cache;
    if (cache?.valid && !state.resyncLatched && (
      checkpoint === null
      || (checkpoint.gridEpoch === "" && checkpoint.seq === 0n)
      || (
        checkpoint.gridEpoch !== "" && checkpoint.gridEpoch === cache.frame.gridEpoch
        && BigInt(cache.frame.seq) > checkpoint.seq
      )
    )) {
      return this.snapshots.seed(socket, sessionId, state.expected.streamId, cache);
    }
    this.snapshots.retry(sessionId, state, "browser checkpoint requires source baseline");
    return false;
  }

  expectStream(sessionId: string, streamId: string, cols: number, rows: number): void {
    const state = this.snapshots.getSession(sessionId);
    if (state.expected?.streamId === streamId
      && state.expected.cols === cols
      && state.expected.rows === rows) return;
    this.snapshots.reset(state, true);
    this.forEachWatcher(sessionId, (socket) => socket.sink.beginTerminalStream(sessionId, streamId));
    this.dropCache(state);
    state.hold.clear();
    state.expected = { streamId, cols, rows };
    state.resyncLatched = false;
  }

  dropSession(sessionId: string): void {
    const detached = detachTerminalScreenWatchers(this.watchersBySession, this.sockets, sessionId);
    const state = this.sessions.get(sessionId);
    if (state) {
      this.snapshots.reset(state, true);
      this.dropCache(state);
      this.sessions.delete(sessionId);
    }
    for (const [, socket] of detached) socket.sink.dropTerminalSession(sessionId);
    detachTerminalScreenWatchers(this.watchersBySession, this.sockets, sessionId);
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

  publishChunk(sessionId: string, chunk: PbCellGridChunk, receivedAtMs = BigInt(this.now())): void {
    const state = this.sessions.get(sessionId);
    if (!state?.expected) return;
    if (chunk.part?.streamId !== state.expected.streamId) return;
    if (chunk.part) chunk.part.sessionId = sessionId;
    stampTerminalSnapshotReceipt(state.chunks, chunk, receivedAtMs);
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
      assertCellGridSnapshot(proto);
      if (proto.cols !== expected.cols || proto.rows !== expected.rows) {
        throw new Error("terminal baseline geometry does not match expected stream");
      }
      const frame = cloneCellGridFrame(normalizeCellGridFrame(protoToCellFrame(proto)));
      this.snapshots.complete(state);
      this.installCache(sessionId, state, frame, countTerminalScreenCacheSpans(frame), proto.coordRecvMs);
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
      if (encodedCellGridFrameSize(proto)
        > CELL_GRID_PART_MAX_BYTES - CELL_GRID_COORD_FANOUT_STAMP_MAX_ENCODED_BYTES) {
        throw new Error("terminal delta leaves no fanout stamp headroom");
      }
      const folded = applyDelta(cloneCellGridFrame(cache.frame), protoToCellFrame(proto));
      if (!folded) throw new Error("terminal delta cannot be folded into baseline");
      normalizeCellGridFrame(folded);
      const spans = countTerminalScreenCacheSpans(folded);
      const rows = folded.rows;
      if (spans > CELL_GRID_SNAPSHOT_MAX_SPANS) throw new Error("terminal cache span limit exceeded");
      if (!this.residency.canReplace(state, rows, spans)) {
        throw new Error("coordinator terminal cache capacity exceeded");
      }
      const nextCache = {
        screen: state,
        frame: folded,
        coordRecvMs: proto.coordRecvMs,
        source: null,
        sourceLeaseCount: 0,
        rows,
        spans,
        valid: true,
      };
      if (!this.residency.replace(state, nextCache)) {
        throw new Error("coordinator terminal cache capacity exceeded");
      }
      state.resyncLatched = false; recordCoordinatorFrame(sessionId, folded, proto, this.watchersBySession);
      const outbound = cellGridEnvelope(clone(PbCellGridFrameSchema, proto));
      this.forEachWatcher(sessionId, (socket, socketId) => {
        const result = socket.sink.enqueueTerminalDelta(
          sessionId,
          expected.streamId,
          outbound,
        );
        if (
          result === "needs_snapshot"
          && this.sockets.get(socketId) === socket
          && socket.watchedSessions.has(sessionId)
        ) this.snapshots.seed(socket, sessionId, expected.streamId, nextCache);
      });
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
    frame: CellGridFrame, spans: number, coordRecvMs: bigint,
  ): void {
    const rows = frame.rows;
    if (!this.residency.canReplace(state, rows, spans)) {
      this.dropCache(state);
      const reason = "coordinator terminal cache capacity exceeded";
      this.unavailable(sessionId, reason);
      signal("terminal.screen_capacity", { session_id: sessionId, rows, spans });
      return;
    }
    const nextCache = {
      screen: state,
      frame,
      coordRecvMs,
      source: null,
      sourceLeaseCount: 0,
      rows,
      spans,
      valid: true,
    };
    if (!this.residency.replace(state, nextCache)) return;
    state.resyncLatched = false; recordCoordinatorFrame(sessionId, frame, frame, this.watchersBySession);
    const streamId = state.expected!.streamId;
    this.options.fullAccepted?.(sessionId, streamId);
    if (state.expected?.streamId !== streamId || state.cache !== nextCache) return;
    this.forEachWatcher(
      sessionId,
      (socket) => this.snapshots.seed(socket, sessionId, streamId, nextCache),
    );
  }

  private forEachWatcher(sessionId: string, callback: (socket: SocketRegistration, socketId: string) => void): void {
    for (const socketId of [...(this.watchersBySession.get(sessionId) ?? [])]) {
      const socket = this.sockets.get(socketId);
      if (socket?.watchedSessions.has(sessionId)) callback(socket, socketId);
    }
  }
  private dropCache(state: SessionScreen): void {
    this.residency.drop(state);
  }
  snapshot(sessionId: string): TerminalScreenSnapshot | null {
    const state = this.sessions.get(sessionId);
    return terminalScreenSnapshot(state?.expected, state?.cache);
  }
}
