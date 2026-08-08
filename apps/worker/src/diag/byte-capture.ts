// Per-session ring buffer of the last 256KB of PTY-output bytes.
// `push(sid, chunk, end_seq)` runs on every chunk appended to a
// session's scrollback (session-manager.ts::appendScrollback). On
// anomaly (worker- or SPA-side detector), `dump(sid, reason)` writes
// the current ring contents + a JSON header to
// ~/Library/Logs/RoostWorker/bytecap-<sid>-<ts>.bin and returns the
// path. LRU caps dumps at 50 files / 500MB.
//
// Header format (single JSON line, terminated by \n):
//   {"sid":"...","ts_ms":1700000000000,"end_seq":12345,"ring_len":262144,"reason":"..."}
// Bytes follow immediately after the newline.
//
// `push` is deliberately always-on and unconditional on ROOST_DIAG: an anomaly
// fires precisely when diag was off, and a dump of an empty ring explains
// nothing. It costs O(chunk) because the ring is a fixed-capacity SbRing.
//
// Owners: worker session-manager.ts (push), worker-anomaly.ts +
// coord diag.snapshot RPC (dump).

import { mkdirSync, writeFileSync, readdirSync, statSync, unlinkSync } from "node:fs";
import { join } from "node:path";
import { diag } from "@roost/shared/diag";
import { workerLogDir } from "@roost/shared/paths";
import { createSbRing, appendToRing, readRing, ringLength, type SbRing } from "../session-scrollback-ring.ts";

const RING_CAP_BYTES = 256 * 1024;
const DUMP_DIR = workerLogDir();
const DUMP_LRU_MAX_FILES = 50;
const DUMP_LRU_MAX_BYTES = 500 * 1024 * 1024;

interface RingEntry {
  ring: SbRing;
  end_seq: number;
}

const _rings = new Map<string, RingEntry>();

/** Append `chunk` to the per-sid ring. Oldest bytes are overwritten in place
 *  once RING_CAP_BYTES is retained. O(chunk), one fixed allocation per sid. */
export function push(sid: string, chunk: Uint8Array, endSeq: number): void {
  let entry = _rings.get(sid);
  if (!entry) {
    entry = { ring: createSbRing(undefined, RING_CAP_BYTES), end_seq: 0 };
    _rings.set(sid, entry);
  }
  appendToRing(entry.ring, chunk);
  entry.end_seq = endSeq;
}

/** Drop the ring for `sid`. Called on session close. */
export function drop(sid: string): void {
  _rings.delete(sid);
}

/** Write the ring to a file. Returns absolute path on success, null on
 *  miss / IO error. Emits diag.byte_dump_written on success. */
export function dump(sid: string, reason: string): string | null {
  const entry = _rings.get(sid);
  if (!entry || ringLength(entry.ring) === 0) {
    diag("diag.byte_dump_written", { sid, reason, written: false, why: "no_ring" });
    return null;
  }
  try {
    mkdirSync(DUMP_DIR, { recursive: true });
  } catch { /* directory exists or unwritable; readdir below will tell us */ }
  // LRU sweep before write so we don't blow past the cap.
  _enforceLruLimits();
  const tsMs = Date.now();
  const sanitizedSid = sid.replace(/[^a-zA-Z0-9_-]/g, "_");
  const path = join(DUMP_DIR, `bytecap-${sanitizedSid}-${tsMs}.bin`);
  const bytes = readRing(entry.ring);
  const header = JSON.stringify({
    sid, ts_ms: tsMs, end_seq: entry.end_seq, ring_len: bytes.length, reason,
  }) + "\n";
  try {
    const out = new Uint8Array(header.length + bytes.length);
    out.set(new TextEncoder().encode(header), 0);
    out.set(bytes, header.length);
    writeFileSync(path, out);
    diag("diag.byte_dump_written", { sid, reason, written: true, path, byte_len: bytes.length, end_seq: entry.end_seq });
    return path;
  } catch (e) {
    diag("diag.byte_dump_written", { sid, reason, written: false, why: "write_error", err: String(e) });
    return null;
  }
}

function _enforceLruLimits(): void {
  let files: { path: string; mtimeMs: number; size: number }[];
  try {
    files = readdirSync(DUMP_DIR)
      .filter((n) => n.startsWith("bytecap-") && n.endsWith(".bin"))
      .map((n) => {
        const p = join(DUMP_DIR, n);
        const s = statSync(p);
        return { path: p, mtimeMs: s.mtimeMs, size: s.size };
      });
  } catch { return; }
  // Oldest first.
  files.sort((a, b) => a.mtimeMs - b.mtimeMs);
  let total = files.reduce((acc, f) => acc + f.size, 0);
  while (files.length > 0 && (files.length >= DUMP_LRU_MAX_FILES || total >= DUMP_LRU_MAX_BYTES)) {
    const victim = files.shift()!;
    try { unlinkSync(victim.path); total -= victim.size; } catch { /* ignore */ }
  }
}

/** Test-only: clear rings + reset state. */
export function _resetForTest(): void {
  _rings.clear();
}

/** Test-only: inspect ring state. */
export function _getRingForTest(sid: string): RingEntry | undefined {
  return _rings.get(sid);
}
