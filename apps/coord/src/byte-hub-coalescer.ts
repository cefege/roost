// Per-session coordinator-internal PTY-byte coalescer. Mirrors the worker's
// cell governor (CELL_EMIT_COALESCE_MS, worker/session-constants.ts):
// LEADING-edge publish gives a single keystroke echo zero added latency, then
// a RE-ARMED fixed interval — never a reset deadline — bounds continuous PTY
// output to one parser batch per window instead of one batch per chunk.
// Order and content are preserved: concatenation is append-only in publish
// order, and both consumers (terminal-title-hub and last-activity-hub) are
// carry-based stream scanners that cannot observe a chunk boundary.

import type { SessionId } from "@roost/shared/wire";
import { globalBytesBus } from "./buses.ts";

const BYTE_COALESCE_MS = 16;
// Bound each pending parser batch and its temporary concatenation allocation.
const BYTE_COALESCE_CAP_BYTES = 256 * 1024;

interface PendingBytes {
  parts: Uint8Array[];
  len: number;
  timer: Timer | undefined;
}

// The coalescer is the sole owner of pending parser batches and their timers.
const pendingBytes = new Map<SessionId, PendingBytes>();

function flushPendingBytes(sessionId: SessionId, pending: PendingBytes): void {
  if (pending.len === 0) return;
  const joined = new Uint8Array(pending.len);
  let at = 0;
  for (const part of pending.parts) {
    joined.set(part, at);
    at += part.length;
  }
  pending.parts = [];
  pending.len = 0;
  globalBytesBus.publish({ session_id: sessionId, bytes: joined });
}

function armByteCoalesce(sessionId: SessionId, pending: PendingBytes): void {
  const timer = setTimeout(() => {
    // Nothing absorbed this window: the session went quiet, so retire the entry
    // and let its next chunk take the leading edge again.
    if (pending.len === 0) {
      if (pendingBytes.get(sessionId) === pending) pendingBytes.delete(sessionId);
      return;
    }
    flushPendingBytes(sessionId, pending);
    armByteCoalesce(sessionId, pending);
  }, BYTE_COALESCE_MS);
  // Never hold the process (or a coord test) open on this timer.
  timer.unref?.();
  pending.timer = timer;
}

export function publishCoalescedBytes(sessionId: SessionId, bytes: Uint8Array): void {
  const pending = pendingBytes.get(sessionId);
  if (!pending) {
    globalBytesBus.publish({ session_id: sessionId, bytes });
    const fresh: PendingBytes = { parts: [], len: 0, timer: undefined };
    pendingBytes.set(sessionId, fresh);
    armByteCoalesce(sessionId, fresh);
    return;
  }
  pending.parts.push(bytes);
  pending.len += bytes.length;
  if (pending.len >= BYTE_COALESCE_CAP_BYTES) {
    clearTimeout(pending.timer);
    flushPendingBytes(sessionId, pending);
    armByteCoalesce(sessionId, pending);
  }
}

/** Discard an undelivered tail and retire its timer when a session closes. */
export function dropCoalescedBytes(sessionId: SessionId): void {
  const pending = pendingBytes.get(sessionId);
  if (!pending) return;
  clearTimeout(pending.timer);
  pendingBytes.delete(sessionId);
}
