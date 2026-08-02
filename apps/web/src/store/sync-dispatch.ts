// Per-session PTY fan-out — bytes / cell-frame / presence dispatch.
// Split out of store/sync.ts (400-line cap). The firehose (_runConnectSync
// in sync.ts) pumps each session's payload here; CellTerminal registers a
// per-session handler on mount and unregisters on unmount. A session with no
// handler drops silently (matches pre-firehose fan-out semantics). Leaf
// module: imports root + shared only, never sync.ts — no import cycle.

import { protoToCellFrame } from "@roost/shared/cell/cell-proto";
import type { CellGridFrame } from "@roost/shared/cell";
import type { PbCellGridFrame } from "@roost/shared/proto/cell_pb";
import { recordCellLag } from "../lib/diag.ts";
import { processOsc8Chunk } from "../lib/terminalOsc8.ts";
// Bytes are live-forward only, for OSC 8 hyperlink tracking. Terminal content
// recovers through immutable cell frames on a later viewport claim.
type PresenceHandler = (msg: unknown) => void;
// R11 cell-grid cell-shipping. CellTerminal (cell mode) registers a per-session
// handler that feeds frames into its CellGridRenderer.
type CellHandler = (frame: CellGridFrame) => void;

const _presenceHandlers = new Map<string, PresenceHandler>();
const _cellHandlers = new Map<string, CellHandler>();
const _cellFrameCounts = new Map<string, number>();
// Full frames only — the repaint a reveal must NOT need (smoke asserts it stays flat).
const _cellFullFrameCounts = new Map<string, number>();
export function registerCellHandler(sessionId: string, fn: CellHandler): () => void {
  _cellHandlers.set(sessionId, fn);
  return () => { if (_cellHandlers.get(sessionId) === fn) _cellHandlers.delete(sessionId); };
}
export function registerPresenceHandler(sessionId: string, fn: PresenceHandler): () => void {
  _presenceHandlers.set(sessionId, fn);
  return () => { if (_presenceHandlers.get(sessionId) === fn) _presenceHandlers.delete(sessionId); };
}

export function _dispatchCell(pb: PbCellGridFrame): void {
  const recvWall = Date.now();
  recordCellLag(pb, recvWall);
  _cellFrameCounts.set(pb.sessionId, (_cellFrameCounts.get(pb.sessionId) ?? 0) + 1);
  if (pb.full === true) _cellFullFrameCounts.set(pb.sessionId, (_cellFullFrameCounts.get(pb.sessionId) ?? 0) + 1);
  const fn = _cellHandlers.get(pb.sessionId);
  if (fn) fn(protoToCellFrame(pb));
}

/** Test-only: how many cell frames have arrived for this session. */
export function cellFrameCount(sessionId: string): number {
  return _cellFrameCounts.get(sessionId) ?? 0;
}

/** Test-only: how many FULL cell frames have arrived for this session. */
export function cellFullFrameCount(sessionId: string): number {
  return _cellFullFrameCounts.get(sessionId) ?? 0;
}

/** Reap a closed session's frame-count entry — keyed by session id with no
 *  other reaper, so it leaks one entry per session ever for the tab's life.
 *  Called from the sessions-delta `closed` handler. */
export function pruneCellFrameCount(sessionId: string): void {
  _cellFrameCounts.delete(sessionId);
  _cellFullFrameCounts.delete(sessionId);
}

/** Live size of the per-session frame-count map — a leak-watch accumulator. */
export function cellFrameCountSize(): number {
  return _cellFrameCounts.size;
}

export function _dispatchBytes(sessionId: string, data: unknown): void {
  let chunk: Uint8Array | null = null;
  if (data instanceof Uint8Array) chunk = data;
  else if (data && typeof data === "object") {
    // A JSON serializer turns Uint8Array into a number-keyed object
    // {0: 65, 1: 66, ...} on the wire. Rehydrate to Uint8Array.
    const keys = Object.keys(data as Record<string, unknown>);
    if (keys.length > 0 && keys.every((k) => /^\d+$/.test(k))) {
      const arr = new Uint8Array(keys.length);
      for (let i = 0; i < keys.length; i++) arr[i] = (data as Record<string, number>)[String(i)] ?? 0;
      chunk = arr;
    }
  } else if (typeof data === "string") {
    chunk = new TextEncoder().encode(data);
  }
  if (!chunk) return;
  // OSC 8 only. OSC 7 (cwd) is parsed by the WORKER, which emits a durable
  // `cwd` SessionEvent that folds into the same store field — a second parser
  // here read the same escape from a volatile, non-backfilled source.
  processOsc8Chunk(sessionId, chunk);
}

export function _dispatchPresence(sessionId: string, data: unknown): void {
  const fn = _presenceHandlers.get(sessionId);
  if (fn) fn(data);
}


