// Transfer state — unifies uploads AND downloads into one M3 card stack.
// Module-level store keyed by id; components read derived values, never mutate.
// Speed is an EMA over per-progress-tick samples; ETA derives from it. Samples
// live in a non-reactive side Map so the rate bookkeeping never churns the
// store. Supersedes store/uploads.ts.
//
// Survives Terminal tab switches because the card is mounted at App-shell level
// (see feedback_persistent_terminal_deck).

import { createStore } from "solid-js/store";

export type TransferDir = "up" | "down";
export type TransferState = "hashing" | "active" | "ok" | "err" | "dedup";

export interface Transfer {
  id: string;
  name: string;
  dir: TransferDir;
  bytes_done: number;
  bytes_total: number;  // 0 = unknown (a download before its first reply)
  speed: number;        // bytes/sec (EMA); 0 = unknown
  eta_s: number;        // seconds remaining; -1 = unknown
  state: TransferState;
  err?: string;
}

interface RateSample { t: number; bytes: number; speed: number; }

const [transfers, setTransfers] = createStore<Record<string, Transfer>>({});
export { transfers };

// Non-reactive rate bookkeeping, keyed by transfer id; dropped on removal.
const samples = new Map<string, RateSample>();

const EMA_ALPHA = 0.4;     // weight on the newest instantaneous rate
const MIN_DELTA_S = 0.05;  // ignore sub-50ms ticks (their instantaneous rate is noise)

export function addTransfer(t: { id: string; name: string; dir: TransferDir; bytes_total: number; state: TransferState }): void {
  setTransfers(t.id, { ...t, bytes_done: 0, speed: 0, eta_s: -1 });
  samples.delete(t.id);
}

/** Live progress. No-op if the card was already dismissed so a late callback
 *  can't resurrect a removed record (per-key write, L11). The rate sample is
 *  seeded on the first tick (so speed measures actual byte flow, not the time
 *  since addTransfer). `nowMs` is injectable purely for deterministic tests. */
export function setTransferProgress(id: string, bytesDone: number, bytesTotal?: number, nowMs = performance.now()): void {
  if (!transfers[id]) return;
  const total = bytesTotal ?? transfers[id]!.bytes_total;
  const prev = samples.get(id);
  if (!prev) {
    samples.set(id, { t: nowMs, bytes: bytesDone, speed: 0 });
    setTransfers(id, { bytes_done: bytesDone, bytes_total: total });
    return;
  }
  const dt = (nowMs - prev.t) / 1000;
  if (dt > MIN_DELTA_S) {
    const inst = (bytesDone - prev.bytes) / dt;
    const speed = prev.speed > 0 ? EMA_ALPHA * inst + (1 - EMA_ALPHA) * prev.speed : inst;
    const eta_s = speed > 0 && total > 0 ? Math.max(0, (total - bytesDone) / speed) : -1;
    setTransfers(id, { bytes_done: bytesDone, bytes_total: total, speed, eta_s });
    samples.set(id, { t: nowMs, bytes: bytesDone, speed });
  } else {
    // Too soon since the last sample — accumulate bytes without a noisy rate.
    setTransfers(id, { bytes_done: bytesDone, bytes_total: total });
  }
}

export function markTransferState(id: string, state: TransferState, err?: string): void {
  if (!transfers[id]) return;
  setTransfers(id, err !== undefined ? { state, err } : { state });
  samples.delete(id);
  // Success/dedup auto-dismiss after 2s; errors persist until the user closes.
  if (state === "ok" || state === "dedup") setTimeout(() => removeTransfer(id), 2000);
}

export function removeTransfer(id: string): void {
  setTransfers(id, undefined as unknown as Transfer);
  samples.delete(id);
}
