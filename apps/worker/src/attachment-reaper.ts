// att1b — attachment reaper. Sweeps `~/.roost/attachments/<session_id>/`
// every hour: deletes files older than 24h and enforces a 1 GB total
// cap (LRU eviction by mtime).
//
// On a fresh worker boot, the base dir may not exist — handle ENOENT
// quietly. Errors are logged but don't crash the worker.

import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import { log } from "@roost/shared/log";

const TTL_MS = 24 * 60 * 60 * 1000;
const SIZE_CAP_BYTES = 1024 * 1024 * 1024;  // 1 GB
const SWEEP_INTERVAL_MS = 60 * 60 * 1000;  // 1h

// The dedup index (hash → filename) lives in each session dir. It must survive
// the TTL/LRU sweep and never surface in the attachment browser listing.
export const MANIFEST_NAME = ".roost-manifest.json";

export function attachmentBaseDir(): string {
  return path.join(os.homedir(), ".roost", "attachments");
}

export function attachmentSessionDir(sessionId: string): string {
  return path.join(attachmentBaseDir(), sessionId);
}

// Path-traversal guard: resolve the session dir and confirm it stays under the
// attachment base (a crafted session_id like "../../etc" must not escape).
// Returns the resolved dir, or null on escape. Single source for the save /
// list / delete sites that each used to inline this check.
export function resolveSessionDirWithinBase(sessionId: string): string | null {
  const baseResolved = path.resolve(attachmentBaseDir());
  const resolved = path.resolve(attachmentSessionDir(sessionId));
  if (resolved === baseResolved || resolved.startsWith(baseResolved + path.sep)) return resolved;
  return null;
}

export function startAttachmentReaper(): { stop: () => void } {
  const timer = setInterval(() => {
    sweepAttachments().catch((e) =>
      log.warn("attachment-reaper", "sweep_failed", { error: String(e) }),
    );
  }, SWEEP_INTERVAL_MS);
  // Initial sweep on boot so workers that have been down >24h don't carry stale data
  void sweepAttachments().catch(() => undefined);
  return { stop: () => clearInterval(timer) };
}

async function sweepAttachments(): Promise<void> {
  const base = attachmentBaseDir();
  if (!fs.existsSync(base)) return;
  const now = Date.now();
  const survivors: Array<{ path: string; size: number; mtime: number }> = [];
  let totalSize = 0;

  for (const sid of fs.readdirSync(base)) {
    const sidDir = path.join(base, sid);
    let stat: fs.Stats;
    try { stat = fs.statSync(sidDir); }
    catch { continue; }
    if (!stat.isDirectory()) continue;

    let files: string[];
    try { files = fs.readdirSync(sidDir); }
    catch { continue; }

    for (const fname of files) {
      if (fname === MANIFEST_NAME) continue;
      const fpath = path.join(sidDir, fname);
      let fstat: fs.Stats;
      try { fstat = fs.statSync(fpath); }
      catch { continue; }
      if (now - fstat.mtimeMs > TTL_MS) {
        try { fs.unlinkSync(fpath); }
        catch (e) { log.warn("attachment-reaper", "unlink_failed", { path: fpath, error: String(e) }); }
      } else {
        survivors.push({ path: fpath, size: fstat.size, mtime: fstat.mtimeMs });
        totalSize += fstat.size;
      }
    }

    // Remove empty session dirs
    try {
      if (fs.readdirSync(sidDir).length === 0) fs.rmdirSync(sidDir);
    } catch { /* ignore — concurrent write may have refilled */ }
  }

  if (totalSize > SIZE_CAP_BYTES) {
    survivors.sort((a, b) => a.mtime - b.mtime);  // oldest first
    let i = 0;
    while (totalSize > SIZE_CAP_BYTES && i < survivors.length) {
      const victim = survivors[i++]!;
      try {
        fs.unlinkSync(victim.path);
        totalSize -= victim.size;
        log.info("attachment-reaper", "lru_evicted", { path: victim.path, size: victim.size });
      } catch (e) {
        log.warn("attachment-reaper", "lru_unlink_failed", { error: String(e) });
      }
    }
  }
}

/** Sanitize an upload filename: strip `/`, NUL, leading dots; truncate to 80
 * chars; preserve extension. Caller prepends a timestamp prefix. */
export function sanitizeAttachmentName(raw: string): string {
  const ext = path.extname(raw);
  const stemMax = Math.max(1, 80 - ext.length);
  let stem = path.basename(raw, ext)
    .replace(/[\/\x00]/g, "")
    .replace(/^\.+/, "")
    .slice(0, stemMax);
  if (!stem) stem = "file";
  return `${stem}${ext}`;
}
