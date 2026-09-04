// Owns terminal baseline repair timers, chunk deadlines, and socket seeding.
// The screen hub delegates here so every retry is tied to the exact session
// state and repair generation that requested it, preventing stale timer
// callbacks from replacing a newer stream or reopening a completed repair.

import { clone } from "@bufbuild/protobuf";
import {
  CellGridChunkAssembler,
  CELL_GRID_CHUNK_STALL_MS,
} from "@roost/shared/cell";
import {
  PbCellGridFrameSchema,
  type PbCellGridFrame,
} from "@roost/shared/proto/cell_pb";
import { diag, signal } from "@roost/shared/diag";
import { terminalSnapshotFrames } from "./terminal-screen-frames.ts";
import {
  TerminalAssemblyHold,
  type SessionScreen,
  type SocketRegistration,
} from "./terminal-screen-hub-state.ts";

export const TERMINAL_SNAPSHOT_FIRST_BYTE_TIMEOUT_MS = CELL_GRID_CHUNK_STALL_MS;

interface TerminalScreenSnapshotControllerOptions {
  sessions: Map<string, SessionScreen>;
  requestSnapshot(sessionId: string, streamId: string): void;
  unavailable(sessionId: string, reason: string): void;
  requestFreshStream(sessionId: string, expectedStreamId: string, reason: string): void;
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
        chunks: {
          assembler: new CellGridChunkAssembler(),
          timer: null,
          timerGeneration: null,
        },
        resyncLatched: false,
        repair: {
          generation: 0,
          requestAttempt: 0,
          requestTimer: null,
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
    proto: PbCellGridFrame,
  ): void {
    try {
      socket.sink.replaceTerminalSnapshot(
        sessionId,
        streamId,
        terminalSnapshotFrames(clone(PbCellGridFrameSchema, proto)),
      );
    } catch (error) {
      const reason = error instanceof Error
        ? error.message
        : "terminal snapshot encoding failed";
      this.options.unavailable(sessionId, reason);
      signal("terminal.snapshot_encode_failed", { session_id: sessionId, reason });
    }
  }

  latch(sessionId: string, state: SessionScreen, reason: string): void {
    this.requestResync(sessionId, state, reason, false);
  }

  retry(sessionId: string, state: SessionScreen, reason: string): void {
    if (state.chunks.assembler.activeSnapshotId !== null) return;
    this.requestResync(sessionId, state, reason, true);
  }

  armChunkTimer(sessionId: string, state: SessionScreen): void {
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
  }

  cancelRequestTimer(state: SessionScreen, resetAttempt: boolean): void {
    if (state.repair.requestTimer) {
      this.options.clearTimer(state.repair.requestTimer);
    }
    state.repair.requestTimer = null;
    if (resetAttempt) state.repair.requestAttempt = 0;
  }

  complete(state: SessionScreen): void {
    this.cancelRequestTimer(state, true);
    this.resetChunks(state);
  }

  reset(state: SessionScreen, advanceGeneration: boolean): void {
    this.cancelRequestTimer(state, true);
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
    timer.unref?.();
    this.options.requestSnapshot(sessionId, streamId);
  }
}
