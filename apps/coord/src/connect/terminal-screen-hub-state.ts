// Defines TerminalScreenHub state, watcher-index mutations, and chunk receipt stamps.
// It is split out of the hub (which sits against the 400-line cap), plus the
// bounded hold that parks ordinary deltas while a chunked baseline assembles.

import { clone } from "@bufbuild/protobuf";
import {
  encodedCellGridFrameSize,
  type CellGridChunkAssembler,
  type CellGridFrame,
} from "@roost/shared/cell";
import {
  PbCellGridFrameSchema,
  type PbCellGridChunk,
  type PbCellGridFrame,
} from "@roost/shared/proto/cell_pb";
import type { TerminalScreenSocketSink } from "./terminal-screen-hub.ts";
import type { TerminalSnapshotSource } from "./terminal-screen-frames.ts";

export interface ExpectedStream { streamId: string; cols: number; rows: number }

export interface ResidentCache {
  readonly screen: SessionScreen;
  frame: CellGridFrame;
  readonly coordRecvMs: bigint;
  source: TerminalSnapshotSource | null;
  sourceLeaseCount: number;
  rows: number;
  spans: number;
  valid: boolean;
}

export interface ChunkState {
  assembler: CellGridChunkAssembler;
  timer: ReturnType<typeof setTimeout> | null;
  timerGeneration: number | null;
  /** First coordinator receipt time for the active chunked snapshot. */
  snapshotCoordRecvMs: bigint | null;
}

export interface SnapshotRepairState {
  generation: number;
  requestAttempt: number;
  requestTimer: ReturnType<typeof setTimeout> | null;
}

export interface SessionScreen {
  expected: ExpectedStream | null;
  cache: ResidentCache | null;
  /** One old canonical source may outlive the current cache. */
  pinnedCache: ResidentCache | null;
  chunks: ChunkState;
  resyncLatched: boolean;
  repair: SnapshotRepairState;
  hold: TerminalAssemblyHold;
}

export interface SocketRegistration {
  sink: TerminalScreenSocketSink;
  watchedSessions: Set<string>;
}

export function attachTerminalScreenWatcher(
  watchersBySession: Map<string, Set<string>>,
  socketId: string,
  socket: SocketRegistration,
  sessionId: string,
): boolean {
  if (socket.watchedSessions.has(sessionId)) return false;
  socket.watchedSessions.add(sessionId);
  const watcherIds = watchersBySession.get(sessionId) ?? new Set<string>();
  watcherIds.add(socketId);
  watchersBySession.set(sessionId, watcherIds);
  return true;
}

export function detachTerminalScreenWatcher(
  watchersBySession: Map<string, Set<string>>,
  socketId: string,
  socket: SocketRegistration,
  sessionId: string,
): boolean {
  if (!socket.watchedSessions.delete(sessionId)) return false;
  const watcherIds = watchersBySession.get(sessionId);
  watcherIds?.delete(socketId);
  if (watcherIds?.size === 0) watchersBySession.delete(sessionId);
  return true;
}

export function detachTerminalScreenSocket(
  watchersBySession: Map<string, Set<string>>,
  sockets: Map<string, SocketRegistration>,
  socketId: string,
  socket: SocketRegistration,
): string[] {
  if (sockets.get(socketId) !== socket) return [];
  const watchedSessionIds = [...socket.watchedSessions];
  sockets.delete(socketId);
  for (const sessionId of watchedSessionIds) {
    detachTerminalScreenWatcher(watchersBySession, socketId, socket, sessionId);
  }
  return watchedSessionIds;
}

export function detachTerminalScreenSockets(
  watchersBySession: Map<string, Set<string>>,
  sockets: Map<string, SocketRegistration>,
): Array<[SocketRegistration, string]> {
  const detached: Array<[SocketRegistration, string]> = [];
  for (const [socketId, socket] of [...sockets]) {
    for (const sessionId of detachTerminalScreenSocket(watchersBySession, sockets, socketId, socket)) {
      detached.push([socket, sessionId]);
    }
  }
  return detached;
}

export function detachTerminalScreenWatchers(
  watchersBySession: Map<string, Set<string>>,
  sockets: Map<string, SocketRegistration>,
  sessionId: string,
): Array<[string, SocketRegistration]> {
  const watcherIds = [...(watchersBySession.get(sessionId) ?? [])];
  watchersBySession.delete(sessionId);
  const detached: Array<[string, SocketRegistration]> = [];
  for (const socketId of watcherIds) {
    const socket = sockets.get(socketId);
    if (!socket?.watchedSessions.delete(sessionId)) continue;
    detached.push([socketId, socket]);
  }
  return detached;
}

/** Parts must share wire metadata, so the first coordinator receipt is canonical. */
export function stampTerminalSnapshotReceipt(
  chunks: ChunkState,
  chunk: PbCellGridChunk,
  receivedAtMs: bigint,
): void {
  const part = chunk.part;
  if (!part) return;
  if (
    chunks.assembler.activeSnapshotId !== chunk.snapshotId
    || chunks.snapshotCoordRecvMs === null
  ) {
    chunks.snapshotCoordRecvMs = receivedAtMs;
  }
  part.coordRecvMs = chunks.snapshotCoordRecvMs ?? receivedAtMs;
}

/** Mirrors the Sync v2 per-domain queue bounds: enough to ride out one baseline transfer. */
export const TERMINAL_SCREEN_ASSEMBLY_HOLD_MAX_FRAMES = 512;
export const TERMINAL_SCREEN_ASSEMBLY_HOLD_MAX_BYTES = 4 * 1024 * 1024;

/**
 * Bounded parking for ordinary deltas that arrive while a chunked baseline
 * assembles. Reaching either cap means the transfer lost the race anyway:
 * push reports false and the caller falls back to the single-resync latch.
 */
export class TerminalAssemblyHold {
  private frames: PbCellGridFrame[] = [];
  private bytes = 0;

  push(frame: PbCellGridFrame): boolean {
    const bytes = encodedCellGridFrameSize(frame);
    if (this.frames.length + 1 > TERMINAL_SCREEN_ASSEMBLY_HOLD_MAX_FRAMES
      || this.bytes + bytes > TERMINAL_SCREEN_ASSEMBLY_HOLD_MAX_BYTES) {
      return false;
    }
    this.frames.push(clone(PbCellGridFrameSchema, frame));
    this.bytes += bytes;
    return true;
  }

  clear(): void {
    this.frames = [];
    this.bytes = 0;
  }

  /**
   * Empties the hold and folds surviving deltas into the just-installed
   * baseline. A held delta whose base_seq no longer matches the live replica
   * is skipped, never folded: the installed full already contains everything
   * emitted before it, so replaying those would only trip the seq guard.
   */
  replay(
    cacheSeq: () => bigint | null,
    live: () => boolean,
    fold: (frame: PbCellGridFrame) => void,
  ): void {
    for (const frame of this.drain()) {
      if (!live()) return;
      const seq = cacheSeq();
      if (seq === null || frame.baseSeq !== seq) continue;
      fold(frame);
    }
  }

  private drain(): PbCellGridFrame[] {
    const frames = this.frames;
    this.frames = [];
    this.bytes = 0;
    return frames;
  }
}
