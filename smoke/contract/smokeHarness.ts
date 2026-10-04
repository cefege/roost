// Type surface of the smoke harness's paint and timing probes: what
// `waitForPaintedMarker`, `waitForPaintedCursor` and the terminal timing
// pair resolve with. Type-only; consumed by smokeTypes.ts and the paint helpers.

import type { CursorPresentationProof, MarkerPresentationProof } from "./terminalDiagSnapshot.ts";

export type PaintedMarkerProof = MarkerPresentationProof;

export type PaintedCursorProof = CursorPresentationProof;

export interface PaintedCursorExpected {
  row?: number;
  column?: number;
}

export type TerminalTimingKind = "trusted_key" | "reveal" | "resize" | "optimistic";

export type TerminalTimingResult = MarkerPresentationProof & {
  timingId: string;
  kind: TerminalTimingKind;
  startedMonotonicMs: number;
  startedEpochMs: number;
  durationMs: number;
  trustedKey: boolean;
};
