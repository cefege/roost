import {
  CELL_GRID_CHUNK_STALL_MS,
} from "@roost/shared/cell";
import type { PbCellGridChunk, PbCellGridFrame } from "@roost/shared/proto/cell_pb";
import type { TerminalSessionReplica } from "./terminal-stream-types.ts";

export function clearTerminalChunkTransfer(session: TerminalSessionReplica): void {
  session.assembler.reset();
  clearTimeout(session.chunkTimer ?? undefined);
  session.chunkTimer = null;
}

export function pushTerminalCellChunk(
  session: TerminalSessionReplica,
  chunk: PbCellGridChunk,
  onComplete: (frame: PbCellGridFrame) => void,
  onInvalid: (reason: string) => void,
): void {
  try {
    const result = session.assembler.push(chunk);
    clearTimeout(session.chunkTimer ?? undefined);
    session.chunkTimer = null;
    if (result.kind === "complete") {
      onComplete(result.frame);
      return;
    }
    session.chunkTimer = setTimeout(() => {
      session.chunkTimer = null;
      if (session.assembler.expire()) {
        onInvalid("terminal snapshot chunk transfer stalled");
      }
    }, CELL_GRID_CHUNK_STALL_MS);
  } catch (error) {
    onInvalid(String(error));
  }
}
