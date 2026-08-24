// Per-session screen-state vocabulary for TerminalScreenHub, split out of it
// (which sits against the 400-line cap), plus the bounded hold that parks
// ordinary deltas while a chunked baseline assembles instead of restarting it.

import { clone } from "@bufbuild/protobuf";
import {
  encodedCellGridFrameSize,
  type CellGridChunkAssembler,
  type CellGridFrame,
} from "@roost/shared/cell";
import { PbCellGridFrameSchema, type PbCellGridFrame } from "@roost/shared/proto/cell_pb";
import type { TerminalScreenSocketSink } from "./terminal-screen-hub.ts";

export interface ExpectedStream { streamId: string; cols: number; rows: number }

export interface ResidentCache {
  frame: CellGridFrame;
  proto: PbCellGridFrame;
  rows: number;
  spans: number;
  valid: boolean;
}

export interface ChunkState {
  assembler: CellGridChunkAssembler;
  timer: ReturnType<typeof setTimeout> | null;
}

export interface SessionScreen {
  expected: ExpectedStream | null;
  cache: ResidentCache | null;
  chunks: ChunkState;
  resyncLatched: boolean;
  hold: TerminalAssemblyHold;
}

export interface SocketRegistration {
  sink: TerminalScreenSocketSink;
  watchedSessions: Set<string>;
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
