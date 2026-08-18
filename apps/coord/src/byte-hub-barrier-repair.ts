// ─── announcement-barrier repair state ────────────────────────────────
//
// Coordinator-local and exact by (workerFp, sessionId, channelId). When the
// announced-channel barrier abandons a buffer, the browser's held cell sequence
// can no longer prove it is current — and the browser has no way to know that,
// so no protocol field carries it. processViewportControl overrides the
// worker-bound held sequence to 0 while a route is marked; publishing a full
// cell frame for that same route is the only thing that clears it.
//
// The channel index this ledger consults lives in byte-hub.ts and stays private
// there: the only read is through `_isChannelBoundToOtherSession`, so a mark
// sweep can never mutate a route.

import type {
  AnnouncedDropReason,
  AnnouncedPhase,
} from "./connect/announced-channel-barrier.ts";
import { _isChannelBoundToOtherSession } from "./byte-hub.ts";
import { signal } from "@roost/shared/diag";

const BARRIER_REPAIR_CAP = 4_096;

export interface BarrierChannelLoss {
  workerFp: string;
  sessionId: string;
  channelId: number;
  reason: AnnouncedDropReason;
  phase: AnnouncedPhase;
  cellFrames: number;
  binaryFrames: number;
  binaryBytes: number;
}

export interface CoordinatorBarrierRepairDiagnostic {
  worker_fp: string;
  channel_id: number;
  reason: AnnouncedDropReason;
  phase: AnnouncedPhase;
  marked_at_ms: number;
  age_ms: number;
  dropped_cell_frames: number;
  dropped_binary_frames: number;
  dropped_binary_bytes: number;
  full_frame_requests: number;
}

export interface CoordinatorAnnouncedBarrierDiagnostic {
  repair_marks: number;
  drops: Record<AnnouncedDropReason, number>;
  dropped_cell_frames: number;
  dropped_binary_frames: number;
  dropped_binary_bytes: number;
  full_frame_requests: number;
}

interface BarrierRepairMark {
  workerFp: string;
  sessionId: string;
  channelId: number;
  reason: AnnouncedDropReason;
  phase: AnnouncedPhase;
  markedAtMs: number;
  droppedCellFrames: number;
  droppedBinaryFrames: number;
  droppedBinaryBytes: number;
  fullFrameRequests: number;
}

const _barrierRepair = new Map<string, BarrierRepairMark>();
const _barrierDrops: Record<AnnouncedDropReason, number> = {
  overflow: 0,
  timeout: 0,
  out_of_order: 0,
  mapping_mismatch: 0,
  superseded: 0,
  append_failed: 0,
  publish_failed: 0,
};
let _barrierDroppedCellFrames = 0;
let _barrierDroppedBinaryFrames = 0;
let _barrierDroppedBinaryBytes = 0;
let _barrierFullFrameRequests = 0;

function _repairKey(workerFp: string, sessionId: string, channelId: number): string {
  return `${workerFp}\u0000${sessionId}\u0000${channelId}`;
}

/** Record one abandoned barrier buffer. Returns true when the route now carries
 *  a repair mark. A `pending` channel is always marked: its durable binding had
 *  not installed yet, so the cell frames arriving after the drop are dropped as
 *  unmapped too — the loss is never limited to what the buffer held. Dropped PTY
 *  bytes get their own signal because a later cell snapshot cannot recreate a
 *  one-time title/OSC-8 mapping. */
export function noteBarrierChannelLoss(loss: BarrierChannelLoss): boolean {
  _barrierDrops[loss.reason] += 1;
  _barrierDroppedCellFrames += loss.cellFrames;
  _barrierDroppedBinaryFrames += loss.binaryFrames;
  _barrierDroppedBinaryBytes += loss.binaryBytes;
  signal("cell.announce_barrier_drop", {
    worker_fp: loss.workerFp,
    sid: loss.sessionId,
    channel_id: loss.channelId,
    reason: loss.reason,
    phase: loss.phase,
    cell_frames: loss.cellFrames,
    cooldownKey: loss.sessionId,
  });
  if (loss.binaryFrames > 0) {
    signal("bytes.metadata_loss", {
      worker_fp: loss.workerFp,
      sid: loss.sessionId,
      channel_id: loss.channelId,
      reason: loss.reason,
      frames: loss.binaryFrames,
      bytes: loss.binaryBytes,
      cooldownKey: loss.sessionId,
    });
  }
  if (loss.cellFrames === 0 && loss.phase !== "pending") return false;
  const key = _repairKey(loss.workerFp, loss.sessionId, loss.channelId);
  const prior = _barrierRepair.get(key);
  if (prior) {
    prior.reason = loss.reason;
    prior.phase = loss.phase;
    prior.markedAtMs = Date.now();
    prior.droppedCellFrames += loss.cellFrames;
    prior.droppedBinaryFrames += loss.binaryFrames;
    prior.droppedBinaryBytes += loss.binaryBytes;
    return true;
  }
  if (_barrierRepair.size >= BARRIER_REPAIR_CAP) {
    const oldest = _barrierRepair.keys().next().value;
    if (oldest !== undefined) _barrierRepair.delete(oldest);
  }
  _barrierRepair.set(key, {
    workerFp: loss.workerFp,
    sessionId: loss.sessionId,
    channelId: loss.channelId,
    reason: loss.reason,
    phase: loss.phase,
    markedAtMs: Date.now(),
    droppedCellFrames: loss.cellFrames,
    droppedBinaryFrames: loss.binaryFrames,
    droppedBinaryBytes: loss.binaryBytes,
    fullFrameRequests: 0,
  });
  return true;
}

export function isBarrierRepairMarked(
  workerFp: string,
  sessionId: string,
  channelId: number,
): boolean {
  return _barrierRepair.has(_repairKey(workerFp, sessionId, channelId));
}

/** Called by publishCellGrid when a FULL frame lands for this exact route —
 *  the only proof the cells the barrier dropped are back. */
export function clearBarrierRepairForFullFrame(
  workerFp: string,
  sessionId: string,
  channelId: number,
): void {
  _barrierRepair.delete(_repairKey(workerFp, sessionId, channelId));
}

/** How many automatic repair claims the coordinator issued for a marked route,
 *  so a live investigation can tell an automatic full frame from a browser one. */
export function noteBarrierRepairFullFrames(
  workerFp: string,
  sessionId: string,
  channelId: number,
  claims: number,
): void {
  if (claims <= 0) return;
  _barrierFullFrameRequests += claims;
  const mark = _barrierRepair.get(_repairKey(workerFp, sessionId, channelId));
  if (mark) mark.fullFrameRequests += claims;
}

/** Called by the channel-index ops after a respawn rebind or an exact-snapshot
 *  replacement. A mark whose route now resolves to a DIFFERENT session can never
 *  be read again — processViewportControl looks marks up by the live route — so
 *  drop it here instead of waiting for the bounded cap. A route that is merely
 *  unbound is kept: its durable binding may still be in flight, and the mark is
 *  exactly what forces that channel's first full frame. */
export function dropStaleBarrierRepair(workerFp: string): void {
  for (const [key, mark] of _barrierRepair) {
    if (mark.workerFp !== workerFp) continue;
    if (!_isChannelBoundToOtherSession(mark.workerFp, mark.channelId, mark.sessionId)) continue;
    _barrierRepair.delete(key);
  }
}

/** Socket teardown. The returning worker re-announces its live set, and that
 *  reconcile snapshot is a producer-generation change which already forces a
 *  fresh full frame for every active owner; marks for a dead connection would
 *  only strand overrides on routes no keeper is producing. */
export function clearBarrierRepairForWorker(workerFp: string): void {
  for (const [key, mark] of _barrierRepair) {
    if (mark.workerFp === workerFp) _barrierRepair.delete(key);
  }
}

export function _barrierRepairSnapshot(
  now = Date.now(),
): Record<string, CoordinatorBarrierRepairDiagnostic[]> {
  const out: Record<string, CoordinatorBarrierRepairDiagnostic[]> = {};
  for (const mark of _barrierRepair.values()) {
    const entry: CoordinatorBarrierRepairDiagnostic = {
      worker_fp: mark.workerFp,
      channel_id: mark.channelId,
      reason: mark.reason,
      phase: mark.phase,
      marked_at_ms: mark.markedAtMs,
      age_ms: Math.max(0, now - mark.markedAtMs),
      dropped_cell_frames: mark.droppedCellFrames,
      dropped_binary_frames: mark.droppedBinaryFrames,
      dropped_binary_bytes: mark.droppedBinaryBytes,
      full_frame_requests: mark.fullFrameRequests,
    };
    const marks = out[mark.sessionId];
    if (marks) marks.push(entry);
    else out[mark.sessionId] = [entry];
  }
  return out;
}

export function _announcedBarrierSnapshot(): CoordinatorAnnouncedBarrierDiagnostic {
  return {
    repair_marks: _barrierRepair.size,
    drops: { ..._barrierDrops },
    dropped_cell_frames: _barrierDroppedCellFrames,
    dropped_binary_frames: _barrierDroppedBinaryFrames,
    dropped_binary_bytes: _barrierDroppedBinaryBytes,
    full_frame_requests: _barrierFullFrameRequests,
  };
}
