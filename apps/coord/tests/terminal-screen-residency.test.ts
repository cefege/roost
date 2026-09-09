// Verifies row/span residency while a lazy terminal snapshot outlives its
// canonical cache. The hub uses this owner before successor installation, so
// slow cursors cannot become invisible to the shared resident-version limit.

import { expect, test } from "bun:test";
import { CellGridChunkAssembler, type CellGridFrame } from "@roost/shared/cell";
import type { PbCellGridFrame } from "@roost/shared/proto/cell_pb";
import {
  TerminalAssemblyHold,
  type ResidentCache,
  type SessionScreen,
} from "../src/connect/terminal-screen-hub-state.ts";
import { TerminalScreenResidency } from "../src/connect/terminal-screen-residency.ts";

function screen(): SessionScreen {
  return {
    expected: null,
    cache: null,
    pinnedCache: null,
    chunks: {
      assembler: new CellGridChunkAssembler(),
      timer: null,
      timerGeneration: null,
    },
    resyncLatched: false,
    repair: {
      generation: 0,
      requestAttempt: 0,
      requestTimer: null,
    },
    hold: new TerminalAssemblyHold(),
  };
}

function cache(screenState: SessionScreen, rows: number): ResidentCache {
  return {
    screen: screenState,
    frame: {} as CellGridFrame,
    proto: {} as PbCellGridFrame,
    source: null,
    sourceLeaseCount: 0,
    rows,
    spans: 1,
    valid: true,
  };
}

test("charges one slow predecessor until every snapshot cursor releases", () => {
  const residency = new TerminalScreenResidency(8, 8);
  const screenState = screen();
  const original = cache(screenState, 4);
  expect(residency.replace(screenState, original)).toBe(true);

  const originalLease = residency.sourceLease(original);
  expect(originalLease.acquire()).toBe(true);
  expect(originalLease.acquire()).toBe(true);
  const successor = cache(screenState, 4);
  expect(residency.replace(screenState, successor)).toBe(true);
  expect(screenState.pinnedCache).toBe(original);

  const successorLease = residency.sourceLease(successor);
  expect(successorLease.acquire()).toBe(true);
  expect(residency.canReplace(screenState, 1, 1)).toBe(false);
  successorLease.release();

  expect(residency.canReplace(screenState, 5, 1)).toBe(false);
  originalLease.release();
  expect(residency.canReplace(screenState, 5, 1)).toBe(false);
  originalLease.release();
  expect(screenState.pinnedCache).toBeNull();
  expect(residency.canReplace(screenState, 5, 1)).toBe(true);
  expect(residency.replace(screenState, cache(screenState, 5))).toBe(true);
  expect(originalLease.acquire()).toBe(false);
});
