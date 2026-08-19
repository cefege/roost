// The resize-sequence floor: everything the worker can PROVE about the keeper's
// applied geometry for one channel. Split out of session-terminal-txn.ts.
//
// Two ways a floor is proven, and the difference between them is the point:
//   resolveResizeFloor     inside a transaction, under an installed capture,
//                          because the caller is about to ALLOCATE a sequence.
//   revalidateResizeFloor  a plain keeper READ — no capture, no allocation, no
//                          geometry change — for a claim whose desired size
//                          already equals proven geometry.
// The floor gates only ALLOCATING a sequence, so a claim that issues no resize
// needs none: an invalid floor is a reason to re-READ the keeper, never a reason
// to freeze the live core and rebuild it at a geometry nothing changed.
//
// `floorProbes` is module-owned state reached only through the accessors below —
// the sanctioned pattern here, cf. the non-exported `viewportSessions` in
// apps/web/src/ws/sync-outbound-viewport-registry.ts. Channel ids are
// per-manager and the map only suppresses a DUPLICATE probe, so a cross-manager
// id collision (unit tests sharing an id) can at worst skip one probe.

import type { SessionManager } from "./session-manager.ts";
import type { KeeperResizeResult } from "./keeper/multiplexed-client.ts";
import { getMultiplexedPool } from "./keeper/multiplexed-client.ts";
import { markResizeBoundary, phaseRemainingMs, type ResizeCapture } from "./session-resize-capture.ts";

/** Hot-path ceiling for proving a floor on a claim that resizes nothing. Under
 *  the keeper's own 2.5 s command watchdog, so a slow keeper degrades to the
 *  full transaction rather than blowing the coordinator's viewport budget. */
export const FLOOR_REVALIDATE_BUDGET_MS = 750;
/** Off-hot-path sweep ceiling: the user never waits on it. */
export const FLOOR_SWEEP_BUDGET_MS = 1_500;

export interface ProvenSize {
  cols: number;
  rows: number;
}

/** Race work against the current phase deadline. The keeper command keeps its
 *  own watchdog; this is the phase's independent monotonic bound, so a hung
 *  socket cannot outlive the transaction. */
export async function withinPhase<T>(capture: ResizeCapture, work: Promise<T>, expired: T): Promise<T> {
  const remaining = phaseRemainingMs(capture);
  if (remaining <= 0) return expired;
  const { promise, resolve } = Promise.withResolvers<T>();
  const timer = setTimeout(() => resolve(expired), remaining);
  try {
    return await Promise.race([work, promise]);
  } finally {
    clearTimeout(timer);
  }
}

/** The sequence floor plus whatever size is currently proven.
 *  `authoritative` = read from the keeper's live channel this pass. */
export interface ResizeFloor {
  seq: number;
  proven: ProvenSize | null;
  authoritative: boolean;
}

/** Recover the floor before allocating. A cached/applied N is advanced past, its
 *  size reconciled, and the local floor can never hand out a sequence the keeper
 *  already consumed — including when the marker that recorded it was evicted,
 *  because the keeper answers from live channel state, not retained history. */
export async function resolveResizeFloor(
  mgr: SessionManager,
  channelId: number,
  capture: ResizeCapture,
): Promise<ResizeFloor> {
  const cached = mgr.channelResizeSeq.get(channelId);
  const provenSize = mgr.lastAppliedSize.get(channelId) ?? null;
  if (cached !== undefined && !mgr.resizeFloorInvalid.has(channelId)) {
    return { seq: cached, proven: provenSize, authoritative: false };
  }
  const pool = getMultiplexedPool();
  const state = await withinPhase(capture, pool.getTerminalState(channelId), null);
  if (state) {
    const size = { cols: state.cols, rows: state.rows };
    mgr.channelResizeSeq.set(channelId, state.highestResizeSeq);
    mgr.resizeFloorInvalid.delete(channelId);
    // appliedResizeSeq 0 = the keeper never applied a sequenced resize, so its
    // dimensions are the spawn geometry and prove nothing about our floor.
    if (state.appliedResizeSeq > 0) mgr.lastAppliedSize.set(channelId, size);
    return {
      seq: state.highestResizeSeq,
      proven: state.appliedResizeSeq > 0 ? size : provenSize,
      authoritative: true,
    };
  }
  if (cached === undefined) {
    // Never observed this channel (worker adoption): derive from ordered history
    // and keep the floor invalid — retained markers can be evicted.
    const history = await withinPhase(capture, pool.getHistoryRecords(channelId), null);
    let latest = 0;
    for (const record of history?.records ?? []) {
      if (record.kind === "resize") latest = Math.max(latest, record.seq);
    }
    mgr.channelResizeSeq.set(channelId, latest);
    mgr.resizeFloorInvalid.add(channelId);
    mgr.pendingCellRepairs.add(channelId);
    return { seq: latest, proven: provenSize, authoritative: false };
  }
  // Floor invalid and authority unreachable: probe the last sequence we wrote.
  // An ACK proves both that it was consumed and at what size; anything else
  // leaves the floor invalid, and allocating cached+1 is still collision-free
  // because a sequence is reserved when its request is WRITTEN.
  const probe = pool.beginResizeStatus(channelId, cached, () => markResizeBoundary(mgr, channelId, capture));
  const result = probe.admission.written
    ? await withinPhase<KeeperResizeResult>(capture, probe.result, { kind: "unknown", seq: cached, reason: "timeout" })
    : { kind: "unknown" as const, seq: cached, reason: "timeout" as const };
  if (result.kind === "ack") {
    const size = { cols: result.cols, rows: result.rows };
    mgr.lastAppliedSize.set(channelId, size);
    mgr.resizeFloorInvalid.delete(channelId);
    return { seq: Math.max(cached, result.seq), proven: size, authoritative: true };
  }
  mgr.pendingCellRepairs.add(channelId);
  return { seq: cached, proven: provenSize, authoritative: false };
}

/** May this channel hand out a sequence without a keeper round trip? The `has`
 *  term is load-bearing: a channel adopted by this worker has no cached seq and
 *  is NOT in `resizeFloorInvalid`, so treating absence as valid would allocate
 *  over a sequence the keeper already consumed. One definition, both gates. */
export function floorValid(mgr: SessionManager, channelId: number): boolean {
  return mgr.channelResizeSeq.has(channelId) && !mgr.resizeFloorInvalid.has(channelId);
}

/** Does this claim ask for the geometry the PTY already has? No sizing viewer
 *  (`null`) counts as unchanged: leave a running TUI at its last size. */
export function sameViewportSize(desired: { cols: number; rows: number } | null, current: ProvenSize): boolean {
  return !desired || (desired.cols === current.cols && desired.rows === current.rows);
}

const floorProbes = new Map<number, Promise<boolean>>();

/** One capture-free keeper READ, so the live core keeps parsing at its proven
 *  size throughout and no gridEpoch is minted. Concurrent callers share one
 *  probe: a user-facing claim awaits the sweep's in-flight read instead of
 *  issuing a second one. `false` — timeout, throw, or a session killed while
 *  the keeper answered — leaves the floor invalid, which is where it started. */
export function revalidateResizeFloor(mgr: SessionManager, channelId: number, budgetMs: number): Promise<boolean> {
  const inflight = floorProbes.get(channelId);
  if (inflight) return inflight;
  const probe = probeResizeFloor(mgr, channelId, budgetMs).finally(() => floorProbes.delete(channelId));
  floorProbes.set(channelId, probe);
  return probe;
}

async function probeResizeFloor(mgr: SessionManager, channelId: number, budgetMs: number): Promise<boolean> {
  if (budgetMs <= 0) return false;
  // Same shape as withinPhase, minus the capture: the deadline is the caller's
  // ceiling, not a phase's, because this probe belongs to no transaction.
  const { promise, resolve } = Promise.withResolvers<null>();
  const timer = setTimeout(() => resolve(null), budgetMs);
  try {
    const work = getMultiplexedPool().getTerminalState(channelId);
    // The deadline can win the race, which would leave a later rejection on the
    // work promise unobserved.
    void work.catch(() => undefined);
    const state = await Promise.race([work, promise]);
    // The await yields: the session can be killed while the keeper answers, and
    // writing here would revive its maps after the kill path cleared them.
    if (!state || !mgr.sessions.has(channelId)) return false;
    mgr.channelResizeSeq.set(channelId, state.highestResizeSeq);
    mgr.resizeFloorInvalid.delete(channelId);
    // Same rule as resolveResizeFloor: appliedResizeSeq 0 means the dimensions
    // are spawn geometry, which proves nothing about applied size.
    if (state.appliedResizeSeq > 0) mgr.lastAppliedSize.set(channelId, { cols: state.cols, rows: state.rows });
    return true;
  } catch {
    return false;
  } finally {
    clearTimeout(timer);
  }
}

/** Prove every stale floor off the hot path, driven by the 5 s viewport
 *  maintenance tick. A floor left invalid by worker adoption or an unresolved
 *  resize ACK is repaired before the user's next claim, so revealing a pane
 *  never pays the keeper round trip. Fire-and-forget: a failed probe leaves the
 *  floor exactly as the sweep found it. */
export function sweepResizeFloors(mgr: SessionManager): void {
  for (const channelId of mgr.sessions.keys()) {
    if (floorValid(mgr, channelId)) continue;
    void revalidateResizeFloor(mgr, channelId, FLOOR_SWEEP_BUDGET_MS);
  }
}
