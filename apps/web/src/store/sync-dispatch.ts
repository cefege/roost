// Per-session PTY fan-out — bytes / cell-frame / presence dispatch.
// Split out of store/sync.ts (400-line cap). The firehose (_runConnectSync
// in sync.ts) pumps each session's payload here; CellTerminal registers a
// per-session handler on mount and unregisters on unmount. A session with no
// handler drops silently (matches pre-firehose fan-out semantics). Leaf
// module: imports root + shared only, never sync.ts — no import cycle.

import { rootStore, setRootStore } from "./root.ts";
import { protoToCellFrame } from "@roost/shared/cell/cell-proto";
import type { CellGridFrame } from "@roost/shared/cell";
import type { PbCellGridFrame } from "@roost/shared/proto/cell_pb";
import { recordCellLag } from "../lib/diag.ts";
import { processOsc8Chunk, pruneOsc8Tracker } from "../lib/terminalOsc8.ts";

// Bytes are live-forward only for OSC 7/8 tracking. Terminal content recovers
// through immutable cell frames on a later viewport claim.
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
 *  Called from the sessions-delta `closed` handler (see pruneOscBuffer). */
export function pruneCellFrameCount(sessionId: string): void {
  _cellFrameCounts.delete(sessionId);
  _cellFullFrameCounts.delete(sessionId);
}

/** Live size of the per-session frame-count map — a leak-watch accumulator. */
export function cellFrameCountSize(): number {
  return _cellFrameCounts.size;
}
// OSC 7 = `ESC ] 7 ; file://<host>/<path> BEL` (or ESC \\ as terminator).
// Shells with TERM_PROGRAM=Apple_Terminal (set by keeper) emit this on every
// chpwd via /etc/zshrc_Apple_Terminal — parse it out of the byte stream and
// live-update the session's cwd in rootStore so the sidebar cwd subline
// tracks `cd`.
// Exported for tests only (parseOsc7Carry.test.ts asserts the carry empties).
export const _oscBuffer = new Map<string, string>();
const OSC7_RE = /\x1b\]7;file:\/\/[^/]*([^\x07\x1b]+)(?:\x07|\x1b\\)/g;
const _osc7Decoder = new TextDecoder("latin1");
function _parseOsc7(sessionId: string, bytes: Uint8Array): void {
  // Fast path: no ESC in this chunk AND no carry → nothing to scan. Skips a
  // TextDecoder.decode + regex sweep on every chunk; OSC 7 only fires on chpwd
  // so the empty path is ~100% common.
  const prior = _oscBuffer.get(sessionId);
  if (!prior && bytes.indexOf(0x1b) === -1) return;
  let text = (prior ?? "") + _osc7Decoder.decode(bytes);
  // Cap buffer so a stream without OSC 7 doesn't grow without bound.
  if (text.length > 4096) text = text.slice(-2048);
  // `g` flag + advancing lastIndex so each .exec advances past the previous
  // match. Without `g`, .exec returns the SAME first match every call — wedges
  // the tab in an infinite loop.
  OSC7_RE.lastIndex = 0;
  let match: RegExpExecArray | null;
  let lastEnd = 0;
  while ((match = OSC7_RE.exec(text)) !== null) {
    let path = match[1] ?? "";
    try { path = decodeURIComponent(path); } catch { /* leave as-is */ }
    if (path) {
      const cur = rootStore.sessions[sessionId];
      if (cur && cur.cwd !== path) setRootStore("sessions", sessionId, "cwd", path);
    }
    lastEnd = OSC7_RE.lastIndex;
  }
  // Carry ONLY an unterminated OSC7 candidate into the next chunk. The old
  // `text.slice(lastEnd) : text` retained the FULL tail whenever nothing
  // matched, so after the first ESC-containing chunk every subsequent chunk
  // paid decode+concat+regex forever (per session, viewed or not). An empty
  // carry restores the no-ESC fast path above.
  const rest = lastEnd > 0 ? text.slice(lastEnd) : text;
  const carry = _osc7CarryOf(rest);
  if (carry) _oscBuffer.set(sessionId, carry);
  else _oscBuffer.delete(sessionId);
}

/** Tail worth carrying to the next chunk: the last `\x1b]7;` candidate with
 *  no BEL / ST terminator after it (an OSC7 split mid-sequence), or a
 *  trailing partial introducer ("\x1b", "\x1b]", "\x1b]7") that the next
 *  chunk could complete. Anything else → "" (drop the buffer). */
function _osc7CarryOf(rest: string): string {
  const start = rest.lastIndexOf("\x1b]7;");
  if (start !== -1) {
    const tail = rest.slice(start);
    // Unterminated candidate → carry it whole. Terminated-but-unmatched
    // (e.g. empty path) → fall through to the partial-introducer check.
    if (!tail.includes("\x07") && !tail.includes("\x1b\\", 1)) return tail;
  }
  for (let n = 3; n >= 1; n--) {
    if (rest.endsWith("\x1b]7;".slice(0, n))) return rest.slice(-n);
  }
  return "";
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
  _parseOsc7(sessionId, chunk);
  processOsc8Chunk(sessionId, chunk);
}

export function _dispatchPresence(sessionId: string, data: unknown): void {
  const fn = _presenceHandlers.get(sessionId);
  if (fn) fn(data);
}

/** Drop a closed session's OSC carry-buffer (keyed by session_id) — otherwise
 *  it leaks one ~2KB string per closed session for the tab's lifetime. Called
 *  by the firehose's sessions-delta handler on `closed` (the right lifecycle
 *  boundary; the mount-scoped handler unregister fires on nav while the session
 *  is still alive). */
export function pruneOscBuffer(sessionId: string): void {
  _oscBuffer.delete(sessionId);
  pruneOsc8Tracker(sessionId);
}
