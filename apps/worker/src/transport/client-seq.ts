// D-4b: persistent monotonic counter for SessionEvent client_seq.
// Survives worker restart so coord's UNIQUE INDEX (worker_fp, client_seq)
// dedup key stays stable across reboots — a worker that successfully
// delivered seq=42 then crashed and restarted resumes at seq=43, not
// seq=1.
//
// Storage: one file per worker data dir, single decimal line.
// Atomic update via write-temp + rename.

import { existsSync, readFileSync, writeFileSync, renameSync, mkdirSync } from "node:fs";
import { join, dirname } from "node:path";
import { workerDataDir } from "@roost/shared/paths";

function defaultPath(): string {
  return join(workerDataDir(), "client-seq.txt");
}

// Watermark step. On boot, we persist `loaded + WATERMARK_STEP` so the
// next WATERMARK_STEP-1 calls hit memory only; on crash, the resumed
// seq is at least WATERMARK_STEP ahead of the last in-flight event so
// coord-side dedup never sees a reused seq (it sees a gap, which is
// fine — coord's UNIQUE INDEX doesn't require contiguity). Cuts disk
// I/O by ~WATERMARK_STEP×; on graceful dispose() we re-persist the
// exact value to minimize the gap.
const WATERMARK_STEP = 1024;

export class ClientSeq {
  private current: number;
  private persisted: number;
  constructor(private path: string = defaultPath()) {
    try { mkdirSync(dirname(this.path), { recursive: true }); } catch { /* ignore */ }
    let loaded = 0;
    if (existsSync(this.path)) {
      const raw = readFileSync(this.path, "utf8").trim();
      const n = Number(raw);
      loaded = Number.isFinite(n) && n >= 0 ? n : 0;
    }
    // Start at the persisted watermark. Until we cross loaded+STEP,
    // every next() is memory-only. The disk file ALREADY reflects this
    // high-water at restart so a crash can't rewind.
    this.current = loaded;
    this.persisted = loaded;
    this.bumpWatermark();
  }

  private bumpWatermark(): void {
    const target = this.current + WATERMARK_STEP;
    const tmp = `${this.path}.tmp`;
    writeFileSync(tmp, String(target));
    renameSync(tmp, this.path);
    this.persisted = target;
  }

  /** Reserve the next sequence number. Persists lazily — disk advances
   *  in WATERMARK_STEP chunks. On worker crash, the next process resumes
   *  AT MOST WATERMARK_STEP-1 seqs ahead of the last in-flight event, so
   *  no seq is ever reused; coord's UNIQUE INDEX tolerates the gap. */
  next(): number {
    this.current += 1;
    if (this.current >= this.persisted) this.bumpWatermark();
    return this.current;
  }

}
