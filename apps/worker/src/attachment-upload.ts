// att1-stream — worker-side streamed-upload assembler. Coord relays the SPA's
// AttachFileChunk stream as ordered DAttachmentChunk frames over the WS; this
// appends each to a temp file under the session's attachment dir and renames
// to the final name on `last`. Memory is O(chunk), not O(file) — the whole
// point of the streaming rebuild (no 50 MB cap). Reply goes back as an rpc-ok
// { abs_path } frame keyed by request_id.
//
// Integrity: each chunk carries a 0-based `seq`. The worker rejects a chunk
// whose seq != the next expected one. This closes the silent-truncation hole —
// if a write fails mid-stream the in-flight state is torn down, so a later
// chunk (seq>0) finds no state and is REFUSED instead of being treated as a
// fresh single-chunk upload and renamed as a truncated file.
//
// Liveness: an idle sweep reaps uploads abandoned with no `last` (SPA crash /
// disconnect) so a stalled upload can't leak an open fd or orphan a temp file.
//
// Called from main.ts's CoordLink onAttachmentChunk wiring. Path-traversal
// guard: the resolved dir must stay under attachmentBaseDir().

import fs from "node:fs";
import path from "node:path";
import { MANIFEST_NAME, resolveSessionDirWithinBase, sanitizeAttachmentName } from "./attachment-reaper.ts";
import { log } from "@roost/shared/log";

export interface AttachmentChunk {
  request_id: string;
  session_id: string;
  filename: string;
  short_path: boolean;
  data: Uint8Array;
  last: boolean;
  seq: number;  // 0-based; must equal the upload's next expected index
}

export interface AttachmentReply {
  ok: (absPath: string) => void;
  err: (message: string) => void;
}

interface InFlight {
  tmpPath: string;
  fd: number;
  filename: string;
  shortPath: boolean;
  dir: string;
  bytesWritten: number;
  nextSeq: number;     // the seq the next chunk for this upload must carry
  lastChunkMs: number; // for the idle reaper
  hasher: Bun.CryptoHasher; // incremental sha256 of the assembled bytes (content-dedup)
}

// request_id → open temp file. Frames for one upload arrive ordered (the WS
// serializes per-socket), so a plain Map without locking is correct.
const uploads = new Map<string, InFlight>();

export function handleAttachmentChunk(chunk: AttachmentChunk, reply: AttachmentReply): void {
  try {
    let inflight = uploads.get(chunk.request_id);
    if (!inflight) {
      // No state for this upload. seq>0 means its state was torn down (a prior
      // chunk errored, or the idle reaper swept it) — REFUSE rather than start
      // fresh, which would rename a truncated file on `last`.
      if (chunk.seq !== 0) { reply.err("upload not in progress (stale or failed chunk)"); return; }
      const dir = resolveSessionDirWithinBase(chunk.session_id);
      if (!dir) {
        reply.err("invalid session_id");
        return;
      }
      fs.mkdirSync(dir, { recursive: true, mode: 0o700 });
      const tmpPath = path.join(dir, `.upload-${chunk.request_id}`);
      const fd = fs.openSync(tmpPath, "w", 0o600);
      inflight = { tmpPath, fd, filename: chunk.filename, shortPath: chunk.short_path, dir, bytesWritten: 0, nextSeq: 0, lastChunkMs: Date.now(), hasher: new Bun.CryptoHasher("sha256") };
      uploads.set(chunk.request_id, inflight);
    }
    if (chunk.seq !== inflight.nextSeq) {
      // Gap / duplicate / reorder — the assembled bytes can no longer be
      // trusted, so abort the whole upload rather than write a corrupt file.
      abortInflight(chunk.request_id, inflight);
      reply.err(`chunk out of order: expected ${inflight.nextSeq}, got ${chunk.seq}`);
      return;
    }
    inflight.nextSeq++;
    inflight.lastChunkMs = Date.now();
    if (chunk.data.length > 0) {
      fs.writeSync(inflight.fd, chunk.data);
      inflight.bytesWritten += chunk.data.length;
      inflight.hasher.update(chunk.data);
    }
    if (chunk.last) {
      fs.fsyncSync(inflight.fd);
      fs.closeSync(inflight.fd);
      uploads.delete(chunk.request_id);
      // Keep the original filename so the injected path reads `foo.png`, not
      // `1782…-foo.png`. Disambiguate only on collision: `foo (2).png`, etc.
      const fname = uniqueName(inflight.dir, sanitizeAttachmentName(inflight.filename));
      const fpath = path.join(inflight.dir, fname);
      fs.renameSync(inflight.tmpPath, fpath);
      recordAttachmentHash(inflight.dir, inflight.hasher.digest("hex"), fname);
      log.info("worker", "attachment_stream_saved", {
        session_id: chunk.session_id, fname, size: inflight.bytesWritten,
      });
      reply.ok(inflight.shortPath ? linkShortPath(inflight.dir, fpath) : fpath);
    }
  } catch (err) {
    const msg = err instanceof Error ? err.message : String(err);
    const inflight = uploads.get(chunk.request_id);
    if (inflight) abortInflight(chunk.request_id, inflight);
    log.warn("worker", "attachment_stream_failed", { request_id: chunk.request_id, error: msg });
    reply.err(msg);
  }
}

// Tear down an upload's temp file + map entry. Idempotent-ish (close/unlink
// swallow already-gone). Used by the error path, the out-of-order guard, and
// the idle reaper.
function abortInflight(requestId: string, inflight: InFlight): void {
  try { fs.closeSync(inflight.fd); } catch { /* already closed */ }
  try { fs.unlinkSync(inflight.tmpPath); } catch { /* never created */ }
  uploads.delete(requestId);
}

// Reap uploads abandoned mid-stream (SPA crash / disconnect with no `last`) so
// a stalled upload can't leak an open fd or orphan a temp file forever. 5 min
// matches coord's pending-RPC deadline — if coord has given up, so should the
// worker. unref() so this never holds the process open (tests + idle workers).
const UPLOAD_IDLE_MS = 5 * 60 * 1000;
setInterval(() => {
  const now = Date.now();
  for (const [requestId, inflight] of uploads) {
    if (now - inflight.lastChunkMs > UPLOAD_IDLE_MS) {
      log.warn("worker", "attachment_stream_abandoned", { request_id: requestId, bytes: inflight.bytesWritten });
      abortInflight(requestId, inflight);
    }
  }
}, 60_000).unref();

// Return `sanitized` if free in `dir`, else insert ` (n)` before the extension
// until free: `foo.png` → `foo (2).png` → `foo (3).png`. Safe without locking —
// the WS delivers frames in order and finalize (this check + renameSync) is
// synchronous, so two uploads can't interleave between the existsSync and the
// rename. `cap` bounds a pathological loop.
function uniqueName(dir: string, sanitized: string): string {
  if (!fs.existsSync(path.join(dir, sanitized))) return sanitized;
  const ext = path.extname(sanitized);           // ".png" ("" if none)
  const stem = sanitized.slice(0, sanitized.length - ext.length);
  for (let n = 2; n < 10_000; n++) {
    const candidate = `${stem} (${n})${ext}`;
    if (!fs.existsSync(path.join(dir, candidate))) return candidate;
  }
  // Pathological: fall back to a timestamp suffix so we never overwrite.
  return `${stem} (${Date.now()})${ext}`;
}

// att2c — optional shorter symlink under <dir>/.shortcuts/pN → real file, so a
// shorter path gets injected into the prompt. Opt-in (roost.useShortAttachPaths).
function linkShortPath(dir: string, fpath: string): string {
  try {
    const shortcutsDir = path.join(dir, ".shortcuts");
    fs.mkdirSync(shortcutsDir, { recursive: true, mode: 0o700 });
    const existing = fs.existsSync(shortcutsDir)
      ? fs.readdirSync(shortcutsDir).filter(n => /^p\d+$/.test(n)) : [];
    const sp = path.join(shortcutsDir, `p${existing.length + 1}`);
    try { fs.unlinkSync(sp); } catch { /* doesn't exist */ }
    fs.symlinkSync(fpath, sp);
    return sp;
  } catch (e) {
    log.warn("worker", "short_path_symlink_failed", { error: String(e) });
    return fpath;
  }
}

// ─── content-dedup manifest ─────────────────────────────────────────────────
// Per-session hash→filename index (`.roost-manifest.json`). The worker records
// each saved upload's sha256 here; a probe (attachment-probe frame) looks a
// hash up so an identical re-upload is skipped and the existing path reused.

function loadManifest(dir: string): Record<string, string> {
  try {
    return JSON.parse(fs.readFileSync(path.join(dir, MANIFEST_NAME), "utf8")) as Record<string, string>;
  } catch {
    return {};
  }
}

function saveManifest(dir: string, m: Record<string, string>): void {
  try {
    fs.writeFileSync(path.join(dir, MANIFEST_NAME), JSON.stringify(m), { mode: 0o600 });
  } catch (e) {
    log.warn("worker", "manifest_write_failed", { dir, error: String(e) });
  }
}

// Record a saved file's content hash. Prunes any stale key pointing at the same
// filename first, so a same-name/different-content overwrite (uniqueName may
// reuse a name after a delete) never leaves a hash mapping to the wrong bytes.
export function recordAttachmentHash(dir: string, sha256: string, fname: string): void {
  const m = loadManifest(dir);
  for (const k of Object.keys(m)) if (m[k] === fname) delete m[k];
  m[sha256] = fname;
  saveManifest(dir, m);
}

// Look up a content hash in a session's manifest. Hit only when the recorded
// file still exists (it may have been reaped since) — a miss makes the SPA fall
// back to a normal upload.
export function probeAttachment(sessionId: string, sha256: string, shortPath: boolean): { hit: boolean; abs_path: string } {
  const dir = resolveSessionDirWithinBase(sessionId);
  if (!dir) return { hit: false, abs_path: "" };
  const fname = loadManifest(dir)[sha256];
  if (!fname) return { hit: false, abs_path: "" };
  const fpath = path.join(dir, fname);
  if (!fs.existsSync(fpath)) return { hit: false, abs_path: "" };
  return { hit: true, abs_path: shortPath ? linkShortPath(dir, fpath) : fpath };
}
