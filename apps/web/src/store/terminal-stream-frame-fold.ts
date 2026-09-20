// Terminal frame folding is shared by the canonical replica and staged direct routes.
// It validates full/delta continuity without knowing about renderers, repairs, or transport.
// Callers own generation, session, and stream admission before invoking these pure transitions.
// Chunk assembly remains outside this module so a staged route can retain its own bounded source.

import {
  CELL_GRID_PART_MAX_BYTES,
  applyDelta,
  encodedCellGridFrameSize,
  normalizeCellGridFrame,
  type CellGridFrame,
} from "@roost/shared/cell";
import { protoToCellFrame } from "@roost/shared/cell/cell-proto";
import type { PbCellGridFrame } from "@roost/shared/proto/cell_pb";

export interface TerminalFrameFoldTarget {
  expectedStreamId: string | null;
  effectiveCols: number;
  effectiveRows: number;
  canonical: CellGridFrame | null;
  baselineReady: boolean;
  assembler: { readonly activeSnapshotId: string | null };
}

export type TerminalFrameFoldFailure =
  | "invalid_full"
  | "full_conflict"
  | "delta_unfollowed"
  | "delta_fold_rejected";

export type TerminalFrameFoldResult =
  | { readonly kind: "full"; readonly frame: CellGridFrame }
  | {
      readonly kind: "delta";
      readonly frame: CellGridFrame;
      readonly canonical: CellGridFrame;
    }
  | { readonly kind: "invalid"; readonly reason: TerminalFrameFoldFailure };

export type TerminalWireFrameDecode =
  | { readonly kind: "frame"; readonly frame: CellGridFrame }
  | { readonly kind: "invalid"; readonly reason: string };

/** Decodes one bounded non-chunked cell frame without mutating a replica. */
export function decodeTerminalWireFrame(
  frame: PbCellGridFrame,
  assembled: boolean,
): TerminalWireFrameDecode {
  if (!assembled && encodedCellGridFrameSize(frame) > CELL_GRID_PART_MAX_BYTES) {
    return {
      kind: "invalid",
      reason: "terminal frame exceeded the encoded part ceiling",
    };
  }
  try {
    return { kind: "frame", frame: protoToCellFrame(frame) };
  } catch (error) {
    return { kind: "invalid", reason: String(error) };
  }
}

/** Applies one already-decoded frame only when it can extend the supplied state. */
export function foldTerminalFrame(
  target: TerminalFrameFoldTarget,
  frame: CellGridFrame,
): TerminalFrameFoldResult {
  return frame.full
    ? foldTerminalFull(target, frame)
    : foldTerminalDelta(target, frame);
}

export function validTerminalFull(
  target: Pick<
    TerminalFrameFoldTarget,
    "expectedStreamId" | "effectiveCols" | "effectiveRows"
  >,
  frame: CellGridFrame,
): boolean {
  if (
    !frame.full
    || frame.baseSeq !== 0
    || frame.streamId !== target.expectedStreamId
    || frame.cols !== target.effectiveCols
    || frame.rows !== target.effectiveRows
    || frame.viewportRows.length !== frame.rows
    || frame.scrollbackAppend.length !== 0
  ) return false;
  for (let index = 0; index < frame.rows; index++) {
    if (frame.viewportRows[index]?.index !== index) return false;
  }
  let historyIndex = frame.sbBase;
  for (const row of frame.scrollbackRows) {
    if (row.index !== historyIndex || row.index >= frame.scrollbackTotal) return false;
    historyIndex++;
  }
  return historyIndex === frame.scrollbackTotal;
}

export function terminalFullFollowsCanonical(
  canonical: CellGridFrame | null,
  frame: CellGridFrame,
): boolean {
  return canonical === null
    || canonical.streamId !== frame.streamId
    || frame.seq > canonical.seq
    || (
      frame.seq === canonical.seq
      && frame.gridEpoch === canonical.gridEpoch
      && frame.cols === canonical.cols
      && frame.rows === canonical.rows
      && frame.altScreen === canonical.altScreen
    );
}

function foldTerminalFull(
  target: TerminalFrameFoldTarget,
  frame: CellGridFrame,
): TerminalFrameFoldResult {
  if (!validTerminalFull(target, frame)) return { kind: "invalid", reason: "invalid_full" };
  if (!terminalFullFollowsCanonical(target.canonical, frame)) {
    return { kind: "invalid", reason: "full_conflict" };
  }
  normalizeCellGridFrame(frame);
  target.canonical = frame;
  target.baselineReady = true;
  return { kind: "full", frame };
}

function foldTerminalDelta(
  target: TerminalFrameFoldTarget,
  delta: CellGridFrame,
): TerminalFrameFoldResult {
  const base = target.canonical;
  if (
    delta.full
    || !target.baselineReady
    || !base
    || target.assembler.activeSnapshotId !== null
    || delta.streamId !== target.expectedStreamId
    || base.streamId !== target.expectedStreamId
    || delta.gridEpoch !== base.gridEpoch
    || delta.cols !== target.effectiveCols
    || delta.rows !== target.effectiveRows
    || delta.baseSeq !== base.seq
    || delta.seq !== delta.baseSeq + 1
  ) return { kind: "invalid", reason: "delta_unfollowed" };

  const canonical = applyDelta(base, delta);
  if (!canonical) return { kind: "invalid", reason: "delta_fold_rejected" };
  normalizeCellGridFrame(canonical);
  target.canonical = canonical;
  return { kind: "delta", frame: delta, canonical };
}
