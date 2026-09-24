// Owns terminal baseline repair timers, chunk deadlines, and socket seeding.
// The screen hub delegates here so every retry is tied to the exact session
// state and repair generation that requested it, preventing stale timer
// callbacks from replacing a newer stream or reopening a completed repair.

import {
  CellGridChunkAssembler,
  CELL_GRID_CHUNK_STALL_MS,
} from "@roost/protocol/cell";
import { diag, signal } from "@roost/observability/diag";
import { log } from "@roost/observability/log";
import type { TerminalSnapshotSource } from "./terminal-screen-frames.ts";
import {
  TerminalAssemblyHold,
  type ResidentCache,
  type SessionScreen,
  type SocketRegistration,
} from "./terminal-screen-hub-state.ts";

export const TERMINAL_SNAPSHOT_FIRST_BYTE_TIMEOUT_MS = CELL_GRID_CHUNK_STALL_MS;

interface TerminalScreenSnapshotControllerOptions {
  sessions: Map<string, SessionScreen>;
  requestSnapshot(sessionId: string, streamId: string): void;
  unavailable(sessionId: string, reason: string): void;
  requestFreshStream(sessionId: string, expectedStreamId: string, reason: string): void;
  snapshotSource(sessionId: string, cache: ResidentCache): TerminalSnapshotSource;
  setTimer(callback: () => void, delayMs: number): NodeJS.Timeout;
  clearTimer(timer: NodeJS.Timeout): void;
  now(): number;
}

export class TerminalScreenSnapshotController {
  constructor(private readonly options: TerminalScreenSnapshotControllerOptions) {}

  getSession(sessionId: string): SessionScreen {
    let state = this.options.sessions.get(sessionId);
    if (!state) {
      state = {
        expected: null,
        cache: null,
        pinnedCache: null,
        chunks: {
          assembler: new CellGridChunkAssembler(),
          timer: null,
          timerGeneration: null,
          snapshotCoordRecvMs: null,
        },
        resyncLatched: false,
        repair: {
          generation: 0,
          requestAttempt: 0,
          requestTimer: null,
          baselineTimer: null,
        },
        hold: new TerminalAssemblyHold(),
      };
      this.options.sessions.set(sessionId, state);
    }
    return state;
  }

  seed(
    socket: SocketRegistration,
    sessionId: string,
    streamId: string,
    cache: ResidentCache,
  ): boolean {
    try {
      const source = cache.source ?? this.options.snapshotSource(sessionId, cache);
      cache.source = source;
      return socket.sink.replaceTerminalSnapshot(sessionId, streamId, source);
    } catch (error) {
      const reason = error instanceof Error
        ? error.message
        : "terminal snapshot encoding failed";
      this.options.unavailable(sessionId, reason);
      signal("terminal.snapshot_encode_failed", { session_id: sessionId, reason });
      return false;
    }
  }

  latch(sessionId: string, state: SessionScreen, reason: string): void {
    this.requestResync(sessionId, state, reason, false);
  }

  retry(sessionId: string, state: SessionScreen, reason: string): void {
    if (state.chunks.assembler.activeSnapshotId !== null) return;
    this.requestResync(sessionId, state, reason, true);
  }

  /**
   * A newly expected stream owns no repair timer, so a worker that commits the
   * stream and installs no baseline leaves the replica holding `expected` with
   * no cache: every watcher sits on an empty pane and nothing repairs it. This
   * deadline hands that stream to the same latch -> snapshot request ->
   * fresh-stream ladder a lost baseline already uses.
   */
  armBaselineTimer(sessionId: string, state: SessionScreen): void {
    this.cancelBaselineTimer(state);
    const expected = state.expected;
    if (!expected) return;
    const generation = state.repair.generation;
    const streamId = expected.streamId;
    const timer = this.options.setTimer(() => {
      if (state.repair.baselineTimer !== timer) return;
      state.repair.baselineTimer = null;
      if (
        this.options.sessions.get(sessionId) !== state
        || state.repair.generation !== generation
        || state.expected?.streamId !== streamId
        || state.cache?.valid === true
        || state.chunks.assembler.activeSnapshotId !== null
      ) return;
      log.warn("terminal-screen", "baseline_timeout", {
        session_id: sessionId,
        stream_id: streamId,
      });
      this.latch(sessionId, state, "terminal stream baseline never arrived");
    }, TERMINAL_SNAPSHOT_FIRST_BYTE_TIMEOUT_MS);
    state.repair.baselineTimer = timer;
    timer.unref?.();
  }

  armChunkTimer(sessionId: string, state: SessionScreen): void {
    // A chunk IS the baseline arriving, so the stall deadline below owns it.
    this.cancelBaselineTimer(state);
    if (state.chunks.timer) this.options.clearTimer(state.chunks.timer);
    const generation = state.repair.generation;
    const timer = this.options.setTimer(() => {
      if (
        state.chunks.timer !== timer
        || state.chunks.timerGeneration !== generation
      ) return;
      state.chunks.timer = null;
      state.chunks.timerGeneration = null;
      if (!state.chunks.assembler.expire(this.options.now())) return;
      state.chunks.snapshotCoordRecvMs = null;
      state.hold.clear();
      this.retry(sessionId, state, "terminal snapshot chunk transfer stalled");
    }, CELL_GRID_CHUNK_STALL_MS);
    state.chunks.timer = timer;
    state.chunks.timerGeneration = generation;
    timer.unref?.();
  }

  resetChunks(state: SessionScreen): void {
    if (state.chunks.timer) this.options.clearTimer(state.chunks.timer);
    state.chunks.timer = null;
    state.chunks.timerGeneration = null;
    state.chunks.assembler.reset();
    state.chunks.snapshotCoordRecvMs = null;
  }

  cancelRequestTimer(state: SessionScreen, resetAttempt: boolean): void {
    if (state.repair.requestTimer) {
      this.options.clearTimer(state.repair.requestTimer);
    }
    state.repair.requestTimer = null;
    if (resetAttempt) state.repair.requestAttempt = 0;
  }

  cancelBaselineTimer(state: SessionScreen): void {
    if (state.repair.baselineTimer) {
      this.options.clearTimer(state.repair.baselineTimer);
    }
    state.repair.baselineTimer = null;
  }

  complete(state: SessionScreen): void {
    this.cancelRequestTimer(state, true);
    this.cancelBaselineTimer(state);
    this.resetChunks(state);
  }

  reset(state: SessionScreen, advanceGeneration: boolean): void {
    this.cancelRequestTimer(state, true);
    this.cancelBaselineTimer(state);
    this.resetChunks(state);
    if (advanceGeneration) state.repair.generation++;
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
    this.startSnapshotRequest(sessionId, state, reason);
  }

  private startSnapshotRequest(
    sessionId: string,
    state: SessionScreen,
    reason: string,
  ): void {
    const expected = state.expected;
    if (
      !expected
      || state.repair.requestTimer !== null
      || state.repair.requestAttempt >= 2
    ) return;
    const generation = state.repair.generation;
    const streamId = expected.streamId;
    state.repair.requestAttempt++;
    const attempt = state.repair.requestAttempt;
    const timer = this.options.setTimer(() => {
      if (state.repair.requestTimer !== timer) return;
      state.repair.requestTimer = null;
      if (
        this.options.sessions.get(sessionId) !== state
        || state.repair.generation !== generation
        || state.expected?.streamId !== streamId
      ) return;
      if (attempt === 1) {
        this.startSnapshotRequest(
          sessionId,
          state,
          "terminal snapshot request produced no bytes",
        );
        return;
      }
      this.options.requestFreshStream(
        sessionId,
        streamId,
        `terminal snapshot repair timed out: ${reason}`,
      );
    }, TERMINAL_SNAPSHOT_FIRST_BYTE_TIMEOUT_MS);
    state.repair.requestTimer = timer;
    // One first-byte deadline per session: this request escalates on its own.
    this.cancelBaselineTimer(state);
    timer.unref?.();
    this.options.requestSnapshot(sessionId, streamId);
  }
}
