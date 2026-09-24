// Chunk transfer owns bounded assembler timers for canonical and staged terminal baselines.
// Callers supply their own progress, repair, and cancellation consequences.
// A completed frame is returned only after the shared assembler has validated every chunk.
// Neither this owner nor a candidate establishes a baseline by receiving partial bytes.

import { CELL_GRID_CHUNK_STALL_MS, CellGridChunkAssembler } from "@roost/protocol/cell";
import type { PbCellGridChunk, PbCellGridFrame } from "@roost/protocol/proto/cell_pb";
import { notifyTerminalBaselineProgress } from "./terminal-stream-progress.ts";
import type { TerminalSessionReplica } from "./terminal-stream-types.ts";

export interface TerminalChunkTransfer {
  assembler: CellGridChunkAssembler;
  chunkTimer: Timer | null;
}

export interface TerminalChunkTransferCallbacks {
  onComplete(frame: PbCellGridFrame): void;
  onInvalid(reason: string): void;
  onProgress(chunk: PbCellGridChunk): void;
  onChange(): void;
}

export function resetTerminalChunkTransfer(transfer: TerminalChunkTransfer): void {
  transfer.assembler.reset();
  clearTimeout(transfer.chunkTimer ?? undefined);
  transfer.chunkTimer = null;
}

export function clearTerminalChunkTransfer(session: TerminalSessionReplica): void {
  resetTerminalChunkTransfer(session);
  notifyTerminalBaselineProgress(session);
}

export function pushTerminalChunkTransfer(
  transfer: TerminalChunkTransfer,
  chunk: PbCellGridChunk,
  callbacks: TerminalChunkTransferCallbacks,
): void {
  try {
    const result = transfer.assembler.push(chunk);
    clearTimeout(transfer.chunkTimer ?? undefined);
    transfer.chunkTimer = null;
    if (result.kind === "complete") {
      callbacks.onChange();
      callbacks.onComplete(result.frame);
      return;
    }
    callbacks.onProgress(chunk);
    callbacks.onChange();
    transfer.chunkTimer = setTimeout(() => {
      transfer.chunkTimer = null;
      if (transfer.assembler.expire()) {
        callbacks.onChange();
        callbacks.onInvalid("terminal snapshot chunk transfer stalled");
      }
    }, CELL_GRID_CHUNK_STALL_MS);
  } catch (error) {
    callbacks.onChange();
    callbacks.onInvalid(String(error));
  }
}

export function pushTerminalCellChunk(
  session: TerminalSessionReplica,
  chunk: PbCellGridChunk,
  onComplete: (frame: PbCellGridFrame) => void,
  onInvalid: (reason: string) => void,
  onProgress: (chunk: PbCellGridChunk) => void,
): void {
  pushTerminalChunkTransfer(session, chunk, {
    onComplete,
    onInvalid,
    onProgress,
    onChange: () => notifyTerminalBaselineProgress(session),
  });
}
