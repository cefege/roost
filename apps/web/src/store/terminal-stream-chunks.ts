// Chunk transfer owns assembler timers and emits bounded progress transitions.
// The replica supplies terminal-frame acceptance and repair callbacks.
// Progress notification never changes wire ordering or baseline atomicity.

import {
  CELL_GRID_CHUNK_STALL_MS,
} from "@roost/shared/cell";
import type { PbCellGridChunk, PbCellGridFrame } from "@roost/shared/proto/cell_pb";
import { notifyTerminalBaselineProgress } from "./terminal-stream-progress.ts";
import type { TerminalSessionReplica } from "./terminal-stream-types.ts";

export function clearTerminalChunkTransfer(session: TerminalSessionReplica): void {
  session.assembler.reset();
  clearTimeout(session.chunkTimer ?? undefined);
  session.chunkTimer = null;
  notifyTerminalBaselineProgress(session);
}

export function pushTerminalCellChunk(
  session: TerminalSessionReplica,
  chunk: PbCellGridChunk,
  onProgress: () => void,
  onComplete: (frame: PbCellGridFrame) => void,
  onInvalid: (reason: string) => void,
): void {
  try {
    const result = session.assembler.push(chunk);
    clearTimeout(session.chunkTimer ?? undefined);
    session.chunkTimer = null;
    notifyTerminalBaselineProgress(session);
    onProgress();
    if (result.kind === "complete") {
      onComplete(result.frame);
      return;
    }
    session.chunkTimer = setTimeout(() => {
      session.chunkTimer = null;
      if (session.assembler.expire()) {
        notifyTerminalBaselineProgress(session);
        onInvalid("terminal snapshot chunk transfer stalled");
      }
    }, CELL_GRID_CHUNK_STALL_MS);
  } catch (error) {
    notifyTerminalBaselineProgress(session);
    onInvalid(String(error));
  }
}
