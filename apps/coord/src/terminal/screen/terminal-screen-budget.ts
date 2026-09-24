// Derives the terminal cell-replica residency ceilings and the per-browser-socket
// send-buffer slice from a declared byte budget. main.ts calls it once at boot and
// hands the result to TerminalViewHub and makeSyncWsHandler; nothing here does I/O,
// so the arithmetic is directly testable. Depends only on the wire-level maxima in
// @roost/protocol plus TerminalScreenHub's hard ceilings.

import { CELL_GRID_PART_MAX_BYTES, CELL_GRID_SNAPSHOT_MAX_SPANS } from "@roost/protocol/cell";
import { TERMINAL_MAX_ROWS } from "@roost/protocol/viewport";
import {
  TERMINAL_SCREEN_MAX_RESIDENT_ROWS,
  TERMINAL_SCREEN_MAX_RESIDENT_SPANS,
} from "./terminal-screen-hub.ts";

/** Retained heap of one charged span: measured at 220 B for a real
 *  unique-text span in Bun 1.3.14, rounded to a 16 B size class. */
export const TERMINAL_SCREEN_BYTES_PER_SPAN = 224;
/** Retained heap of one charged row: PbCellRow object 48 B + parent array slot
 *  8 B + the row's own spans array header 32 B. */
export const TERMINAL_SCREEN_BYTES_PER_ROW = 88;
/** Every charged replica is mirrored once, UNCHARGED, by ResidentCache.source
 *  (terminal-screen-frames.ts materializedSource) for as long as the cache
 *  lives, so a budget must reserve two copies of everything it admits. */
export const TERMINAL_SCREEN_REPLICA_COPIES = 2;
/** Rows are ~2% of replica cost; this split keeps the row cap meaningful
 *  without letting it consume span budget. */
export const TERMINAL_SCREEN_ROW_BUDGET_FRACTION = 0.05;
/** Share of the detected ceiling the replica pool may claim. */
export const TERMINAL_SCREEN_BUDGET_CEILING_FRACTION = 0.25;

/** A browser socket's real send buffer, sized so eight concurrent viewers
 *  cannot exceed the replica budget. Floor: two whole snapshot parts. */
export const SYNC_BACKPRESSURE_BUDGET_SOCKETS = 8;
const SYNC_BACKPRESSURE_MAX_BYTES = 8 * 1024 * 1024;

export interface TerminalScreenCaps {
  maxResidentRows: number;
  maxResidentSpans: number;
}

function clamp(value: number, low: number, high: number): number {
  return Math.min(Math.max(value, low), high);
}

/** Budget bytes → the two residency ceilings, clamped between "one
 *  worst-styled max-geometry session must always fit" and today's constants. */
export function terminalScreenCaps(budgetBytes: number): TerminalScreenCaps {
  const effective = Math.floor(budgetBytes / TERMINAL_SCREEN_REPLICA_COPIES);
  const rowBytes = Math.floor(effective * TERMINAL_SCREEN_ROW_BUDGET_FRACTION);
  const spanBytes = effective - rowBytes;
  return {
    maxResidentRows: clamp(
      Math.floor(rowBytes / TERMINAL_SCREEN_BYTES_PER_ROW),
      TERMINAL_MAX_ROWS,
      TERMINAL_SCREEN_MAX_RESIDENT_ROWS,
    ),
    maxResidentSpans: clamp(
      Math.floor(spanBytes / TERMINAL_SCREEN_BYTES_PER_SPAN),
      CELL_GRID_SNAPSHOT_MAX_SPANS,
      TERMINAL_SCREEN_MAX_RESIDENT_SPANS,
    ),
  };
}

/** 25% of the cgroup/host ceiling unless the operator declared a budget. */
export function terminalScreenBudgetBytes(
  configuredBudgetBytes: number | undefined,
  ceilingBytes: number,
): number {
  if (configuredBudgetBytes !== undefined) return configuredBudgetBytes;
  return Math.floor(ceilingBytes * TERMINAL_SCREEN_BUDGET_CEILING_FRACTION);
}

export function syncBackpressureBytes(budgetBytes: number): number {
  return clamp(
    Math.floor(budgetBytes / SYNC_BACKPRESSURE_BUDGET_SOCKETS),
    2 * CELL_GRID_PART_MAX_BYTES,
    SYNC_BACKPRESSURE_MAX_BYTES,
  );
}
