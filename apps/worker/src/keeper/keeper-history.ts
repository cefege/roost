// Resize-aware history assembly for the multiplexed keeper's GetHistory*
// replies. The output ring alone loses window geometry; the resize markers
// interleaved with retained bytes are what let a resuming worker replay the
// exact byte/geometry stream the PTY produced. Called from the frame handler;
// owns nothing — every channel field here belongs to the pool's channels map.

import {
  KEEPER_MAX_HISTORY_RESIZE_RECORDS,
} from "./protocol.ts";
import type { KeeperHistoryRecord, KeeperHistoryRecords } from "./protocol.ts";
import type { Channel } from "./keeper-types.ts";
import { readRing, ringLength } from "../session-scrollback-ring.ts";

const NO_RETAINED_BYTES = new Uint8Array(0);

export function trimEvictedResizeHistory(ch: Channel): void {
  const retainedTail = ch.headSeq - ringLength(ch.outRing);
  let removeCount = 0;
  while (removeCount < ch.historyResizes.length
      && ch.historyResizes[removeCount]!.headSeq <= retainedTail) {
    const evicted = ch.historyResizes[removeCount]!;
    ch.historyBaseCols = evicted.cols;
    ch.historyBaseRows = evicted.rows;
    removeCount++;
  }
  if (removeCount > 0) ch.historyResizes.splice(0, removeCount);
}

export function appendResizeHistory(ch: Channel, seq: number, cols: number, rows: number): void {
  // A resize-only flood must not grow an unbounded marker list. If its marker
  // budget is exhausted, discard the raw window too; retaining fewer records
  // is truthful, while retaining bytes under an unknowable geometry is not.
  if (ch.historyResizes.length >= KEEPER_MAX_HISTORY_RESIZE_RECORDS) {
    ch.outRing.buf = NO_RETAINED_BYTES;
    ch.outRing.write = 0;
    ch.outRing.filled = 0;
    ch.historyResizes.length = 0;
    ch.historyBaseCols = ch.currentCols;
    ch.historyBaseRows = ch.currentRows;
  }
  ch.historyResizes.push({ headSeq: ch.headSeq, seq, cols, rows });
  ch.currentCols = cols;
  ch.currentRows = rows;
}

export function orderedHistory(ch: Channel): KeeperHistoryRecords {
  const retained = readRing(ch.outRing);
  const retainedTail = ch.headSeq - retained.byteLength;
  const records: KeeperHistoryRecord[] = [];
  let rawSeq = retainedTail;
  for (const resize of ch.historyResizes) {
    if (resize.headSeq < retainedTail || resize.headSeq > ch.headSeq) continue;
    const outputBytes = resize.headSeq - rawSeq;
    if (outputBytes > 0) {
      const offset = rawSeq - retainedTail;
      records.push({ kind: "output", bytes: retained.subarray(offset, offset + outputBytes) });
      rawSeq = resize.headSeq;
    }
    records.push({
      kind: "resize",
      seq: resize.seq,
      cols: resize.cols,
      rows: resize.rows,
    });
  }
  if (rawSeq < ch.headSeq) {
    records.push({ kind: "output", bytes: retained.subarray(rawSeq - retainedTail) });
  }
  return {
    headSeq: ch.headSeq,
    baseCols: ch.historyBaseCols,
    baseRows: ch.historyBaseRows,
    records,
  };
}
