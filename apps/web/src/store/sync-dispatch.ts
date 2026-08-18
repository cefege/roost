// Per-session terminal metadata / cell-frame / presence dispatch.
import { markPhaseOnce } from "../lib/diag.ts";
// Split out of store/sync.ts. The Sync firehose pumps each session payload
// here; CellTerminal registers cell and presence handlers on mount. Compact
// terminal-link mappings are retained even when no pane is mounted.

import { toBinary } from "@bufbuild/protobuf";
import { protoToCellFrame } from "@roost/shared/cell/cell-proto";
import type { CellGridFrame } from "@roost/shared/cell";
import {
  PbCellGridFrameSchema,
  type PbCellGridFrame,
} from "@roost/shared/proto/cell_pb";
import { recordCellLag } from "../lib/diag.ts";
import { rootStore } from "./root.ts";
type PresenceHandler = (msg: unknown) => void;
// R11 cell-grid cell-shipping. CellTerminal (cell mode) registers a per-session
// handler that feeds frames into its CellGridRenderer.
type CellHandler = (frame: CellGridFrame) => void;

const _presenceHandlers = new Map<string, PresenceHandler>();
const _cellHandlers = new Map<string, CellHandler>();
const _cellFrameCounts = new Map<string, number>();
// Full frames only — the repaint a reveal must NOT need (smoke asserts it stays flat).
const _cellFullFrameCounts = new Map<string, number>();
// Historical rows carried by the last authoritative full frame. Viewport-only
// resume keeps this at zero; demand-driven pages do not affect the counter.
const _cellFullFrameSbRows = new Map<string, number>();
const _cellGridEpochs = new Map<string, string>();
const _dropNextCellFrames = new Set<string>();
const _droppedCellFrameCounts = new Map<string, number>();
const _cellWireGridEpochs = new Map<string, string>();
const _cellWireSeqs = new Map<string, number>();

// Opened can enter the reactive store one turn before TerminalDeck mounts its
// CellTerminal. Only store-known sessions with a live browser claim may bridge
// that gap. Every buffer starts at an authoritative full frame.
const MOUNT_BUFFER_MAX_FRAMES = 64;
const MOUNT_BUFFER_MAX_BYTES = 4 * 1024 * 1024;
const MOUNT_BUFFER_MAX_MS = 3_000;
interface CellMountBuffer {
  frames: PbCellGridFrame[];
  bytes: number;
  timer: ReturnType<typeof setTimeout>;
}
interface CellMountClaimState {
  active: boolean;
}
const _cellMountClaims = new Map<string, CellMountClaimState>();
const _cellMountBuffers = new Map<string, CellMountBuffer>();
const _cellMountRepairPending = new Set<string>();

function clearCellMountBuffer(sessionId: string, requestRepair: boolean): void {
  const pending = _cellMountBuffers.get(sessionId);
  if (pending) {
    clearTimeout(pending.timer);
    _cellMountBuffers.delete(sessionId);
  }
  if (requestRepair) _cellMountRepairPending.add(sessionId);
}

function forceClearCellMountClaim(sessionId: string): void {
  _cellMountClaims.delete(sessionId);
  clearCellMountBuffer(sessionId, false);
  _cellMountRepairPending.delete(sessionId);
}

/** A tokenized claim for the short gap between viewport ownership and mount. */
export interface CellMountClaim {
  /** Admit mount-gap buffering once this owner has published its viewport. */
  activate(): void;
  /** Stop buffering while keeping this owner reusable for a later reveal. */
  deactivate(): void;
  /** Permanently release this owner. Stale and repeated releases are inert. */
  release(): void;
}

/** Acquire the latest mount-gap owner for a session.
 *
 * Acquisition supersedes any older handle without disturbing buffered state:
 * the successor inherits the same mount gap. Only the current handle may
 * activate, deactivate, or clear that state.
 */
export function acquireCellMountClaim(sessionId: string): CellMountClaim {
  const state: CellMountClaimState = { active: false };
  _cellMountClaims.set(sessionId, state);
  let released = false;
  return {
    activate(): void {
      if (!released && _cellMountClaims.get(sessionId) === state) {
        state.active = true;
      }
    },
    deactivate(): void {
      if (released || _cellMountClaims.get(sessionId) !== state) return;
      state.active = false;
      clearCellMountBuffer(sessionId, false);
      _cellMountRepairPending.delete(sessionId);
    },
    release(): void {
      if (released) return;
      released = true;
      if (_cellMountClaims.get(sessionId) !== state) return;
      forceClearCellMountClaim(sessionId);
    },
  };
}

function bufferCellForMount(pb: PbCellGridFrame): void {
  const sessionId = pb.sessionId;
  // Unknown and inactive sessions deliberately remain live-drop. A Sync seed
  // cannot make an unclaimed terminal start retaining fleet-wide cell traffic.
  if (!rootStore.sessions[sessionId] || !_cellMountClaims.get(sessionId)?.active) return;

  let pending = _cellMountBuffers.get(sessionId);
  if (pb.full) {
    if (pb.cols <= 0 || pb.rows <= 0 || pb.seq <= 0n) {
      clearCellMountBuffer(sessionId, true);
      return;
    }
    // A newer authoritative full supersedes an older not-yet-mounted chain.
    clearCellMountBuffer(sessionId, false);
    const timer = setTimeout(() => {
      if (_cellMountBuffers.get(sessionId) === pending) {
        clearCellMountBuffer(sessionId, true);
      }
    }, MOUNT_BUFFER_MAX_MS);
    pending = { frames: [], bytes: 0, timer };
    _cellMountBuffers.set(sessionId, pending);
  } else {
    if (!pending) {
      _cellMountRepairPending.add(sessionId);
      return;
    }
    const priorSeq = pending.frames.at(-1)?.seq ?? 0n;
    if (pb.seq !== priorSeq + 1n) {
      clearCellMountBuffer(sessionId, true);
      return;
    }
  }

  const encodedBytes = toBinary(PbCellGridFrameSchema, pb).byteLength;
  if (
    !pending
    || pending.frames.length >= MOUNT_BUFFER_MAX_FRAMES
    || pending.bytes + encodedBytes > MOUNT_BUFFER_MAX_BYTES
  ) {
    clearCellMountBuffer(sessionId, true);
    return;
  }
  pending.frames.push(pb);
  pending.bytes += encodedBytes;
}
function smokeDropKey(sessionId: string): string {
  return `roostSmoke.dropCell.${sessionId}`;
}
// ONE module-load read. The persisted-drop backdoor is only ever armed by the
// smoke harness, which sets localStorage.roostSmoke="1" and THEN reloads, so a
// module-load read sees exactly what a per-frame read would. Per-frame it put
// two synchronous localStorage calls on production's paint path.
const _smokeEnabled = typeof localStorage !== "undefined" && localStorage.getItem("roostSmoke") === "1";

export function dropNextCellFrame(sessionId: string): void {
  if (typeof localStorage === "undefined" || localStorage.getItem("roostSmoke") !== "1") return;
  _dropNextCellFrames.add(sessionId);
  localStorage.setItem(smokeDropKey(sessionId), "1");
}
export function droppedCellFrameCount(sessionId: string): number {
  return _droppedCellFrameCounts.get(sessionId) ?? 0;
}
export function registerCellHandler(
  sessionId: string,
  fn: CellHandler,
  requestFullRepair?: () => void,
): () => void {
  // Drain the retained full→delta chain in this same call stack before the
  // handler becomes visible to newer Sync frames.
  const pending = _cellMountBuffers.get(sessionId);
  if (pending) {
    clearTimeout(pending.timer);
    _cellMountBuffers.delete(sessionId);
    for (const frame of pending.frames) fn(protoToCellFrame(frame));
  }
  _cellHandlers.set(sessionId, fn);
  if (_cellMountRepairPending.delete(sessionId)) requestFullRepair?.();
  return () => {
    if (_cellHandlers.get(sessionId) === fn) _cellHandlers.delete(sessionId);
  };
}
export function registerPresenceHandler(sessionId: string, fn: PresenceHandler): () => void {
  _presenceHandlers.set(sessionId, fn);
  return () => { if (_presenceHandlers.get(sessionId) === fn) _presenceHandlers.delete(sessionId); };
}

export function _dispatchCell(pb: PbCellGridFrame): void {
  markPhaseOnce("first_cell_receive", pb.sessionId, {
    sessionId: pb.sessionId,
    sequence: pb.seq,
    full: pb.full,
  });
  // Wire receipt is distinct from smoke drops, mount buffering, handler
  // admission, and renderer reconciliation. Keep only one scalar watermark
  // per live session; no frame history or per-frame diagnostic allocation.
  _cellWireGridEpochs.set(pb.sessionId, pb.gridEpoch);
  _cellWireSeqs.set(pb.sessionId, Number(pb.seq));
  const persistedDrop = _smokeEnabled
    && localStorage.getItem(smokeDropKey(pb.sessionId)) === "1";
  if (persistedDrop) localStorage.removeItem(smokeDropKey(pb.sessionId));
  const runtimeDrop = _dropNextCellFrames.delete(pb.sessionId);
  if (persistedDrop || runtimeDrop) {
    _droppedCellFrameCounts.set(
      pb.sessionId,
      (_droppedCellFrameCounts.get(pb.sessionId) ?? 0) + 1,
    );
    return;
  }
  const recvWall = Date.now();
  recordCellLag(pb, recvWall);
  _cellFrameCounts.set(pb.sessionId, (_cellFrameCounts.get(pb.sessionId) ?? 0) + 1);
  _cellGridEpochs.set(pb.sessionId, pb.gridEpoch);
  if (pb.full === true) {
    _cellFullFrameCounts.set(pb.sessionId, (_cellFullFrameCounts.get(pb.sessionId) ?? 0) + 1);
    _cellFullFrameSbRows.set(pb.sessionId, pb.scrollbackRows.length);
  }
  const fn = _cellHandlers.get(pb.sessionId);
  if (fn) {
    fn(protoToCellFrame(pb));
    return;
  }
  bufferCellForMount(pb);
}

/** Test-only: how many cell frames have arrived for this session. */
export function cellFrameCount(sessionId: string): number {
  return _cellFrameCounts.get(sessionId) ?? 0;
}

/** Test-only: how many FULL cell frames have arrived for this session. */
export function cellFullFrameCount(sessionId: string): number {
  return _cellFullFrameCounts.get(sessionId) ?? 0;
}

/** Test-only: scrollback rows on the last FULL frame — the claim snapshot's
 *  size, which must not scale with history depth or the viewer's gap. */
export function lastFullFrameSbRows(sessionId: string): number {
  return _cellFullFrameSbRows.get(sessionId) ?? -1;
}

/** Test-only: opaque epoch on the most recently received cell frame. */
export function cellGridEpoch(sessionId: string): string {
  return _cellGridEpochs.get(sessionId) ?? "";
}

export interface CellWireEpochSeq {
  grid_epoch: string | null;
  seq: number | null;
}

/** Latest cell frame decoded from Sync, before any browser-local drop/handler. */
export function cellWireEpochSeq(sessionId: string): CellWireEpochSeq {
  return {
    grid_epoch: _cellWireGridEpochs.get(sessionId) ?? null,
    seq: _cellWireSeqs.get(sessionId) ?? null,
  };
}

/** Reap a closed session's frame-count entry — keyed by session id with no
 *  other reaper, so it leaks one entry per session ever for the tab's life.
 *  Called from the sessions-delta `closed` handler. */
export function pruneCellFrameCount(sessionId: string): void {
  _cellFrameCounts.delete(sessionId);
  _cellFullFrameCounts.delete(sessionId);
  _cellFullFrameSbRows.delete(sessionId);
  _cellGridEpochs.delete(sessionId);
  _cellWireGridEpochs.delete(sessionId);
  _cellWireSeqs.delete(sessionId);
  _dropNextCellFrames.delete(sessionId);
  if (typeof localStorage !== "undefined") localStorage.removeItem(smokeDropKey(sessionId));
  _droppedCellFrameCounts.delete(sessionId);
  forceClearCellMountClaim(sessionId);
}

/** Live size of the per-session frame-count map — a leak-watch accumulator. */
export function cellFrameCountSize(): number {
  return _cellFrameCounts.size;
}

export interface CellMountBufferStats {
  sessions: number;
  frames: number;
  bytes: number;
  pendingRepairs: number;
}

/** Deterministic diagnostics/test seam for the explicit mount-gap bounds. */
export function cellMountBufferStats(): CellMountBufferStats {
  let frames = 0;
  let bytes = 0;
  for (const pending of _cellMountBuffers.values()) {
    frames += pending.frames.length;
    bytes += pending.bytes;
  }
  return {
    sessions: _cellMountBuffers.size,
    frames,
    bytes,
    pendingRepairs: _cellMountRepairPending.size,
  };
}

/** Sync generation reset: buffered frames belong to the old socket and can
 * never cross into the next generation. Keep each latest owner token but make
 * it inactive; its next positive viewport publication reactivates it. */
export function resetCellMountBuffers(): void {
  for (const pending of _cellMountBuffers.values()) clearTimeout(pending.timer);
  for (const claim of _cellMountClaims.values()) claim.active = false;
  _cellMountBuffers.clear();
  _cellMountRepairPending.clear();
}

export function _dispatchPresence(sessionId: string, data: unknown): void {
  const fn = _presenceHandlers.get(sessionId);
  if (fn) fn(data);
}


