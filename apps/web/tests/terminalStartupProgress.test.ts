// Locks the startup meter's observable contract: contiguous ordered bands, a
// creep that stays inside its own band, chunk subdivision that can overtake but
// never escape, a floor that absorbs status regressions, and the 99 ceiling
// that keeps 100% reserved for completion.

import { describe, expect, test } from "bun:test";
import {
  TERMINAL_STARTUP_STEPS,
  terminalStartupChunkDetail,
  terminalStartupCompletesJourney,
  terminalStartupPercent,
  type TerminalStartupStage,
} from "../src/lib/terminalStartupProgress.ts";

const ORDERED_STAGES: TerminalStartupStage[] = [
  "identity", "sync", "sessions", "spawn", "measure", "viewport", "frame", "render",
];

describe("terminal startup bands", () => {
  test("the eight forward stages tile 4→99 with no gap or overlap", () => {
    let cursor = TERMINAL_STARTUP_STEPS.identity.start;
    expect(cursor).toBe(4);
    for (const stage of ORDERED_STAGES) {
      const step = TERMINAL_STARTUP_STEPS[stage];
      expect(step.start).toBe(cursor);
      expect(step.end).toBeGreaterThan(step.start);
      cursor = step.end;
    }
    expect(cursor).toBe(99);
  });

  test("retry is zero-width so a reconnecting step cannot advance", () => {
    expect(TERMINAL_STARTUP_STEPS.retry.start).toBe(TERMINAL_STARTUP_STEPS.retry.end);
    expect(terminalStartupPercent({ stage: "retry", stageElapsedMs: 5_000, floor: 91 }))
      .toBe(91);
  });
});

describe("terminalStartupPercent", () => {
  test("a waiting step creeps forward but never leaves its own band", () => {
    expect(terminalStartupPercent({ stage: "sessions", stageElapsedMs: 0, floor: 0 }))
      .toBe(30);
    const nearlyOneTimeConstant = terminalStartupPercent({
      stage: "sessions",
      stageElapsedMs: 900,
      floor: 0,
    });
    expect(nearlyOneTimeConstant).toBeGreaterThan(39);
    expect(nearlyOneTimeConstant).toBeLessThan(46);
    expect(terminalStartupPercent({ stage: "sessions", stageElapsedMs: 60_000, floor: 0 }))
      .toBeLessThan(46);
  });

  test("chunked assembly subdivides the frame band and outruns the creep", () => {
    expect(terminalStartupPercent({
      stage: "frame",
      stageElapsedMs: 0,
      chunks: { received: 7, total: 7 },
      floor: 0,
    })).toBe(96);
    expect(terminalStartupPercent({
      stage: "frame",
      stageElapsedMs: 0,
      chunks: { received: 0, total: 7 },
      floor: 0,
    })).toBe(82);
  });

  test("a racing or garbage chunk count cannot leave the band", () => {
    expect(terminalStartupPercent({
      stage: "frame",
      stageElapsedMs: 0,
      chunks: { received: 9, total: 7 },
      floor: 0,
    })).toBe(96);
    expect(terminalStartupPercent({
      stage: "frame",
      stageElapsedMs: 0,
      chunks: { received: -2, total: 5 },
      floor: 0,
    })).toBe(82);
    // Unusable total falls back to pure time creep, which at 0ms is the floor
    // of the band rather than a NaN width.
    expect(terminalStartupPercent({
      stage: "frame",
      stageElapsedMs: 0,
      chunks: { received: 1, total: Number.NaN },
      floor: 0,
    })).toBe(82);
  });

  test("the floor absorbs a stage regression instead of rewinding the bar", () => {
    expect(terminalStartupPercent({ stage: "measure", stageElapsedMs: 0, floor: 88 }))
      .toBe(88);
  });

  test("nothing reaches 100 — only completion does", () => {
    expect(terminalStartupPercent({ stage: "render", stageElapsedMs: 600_000, floor: 99 }))
      .toBe(99);
  });
});

describe("terminalStartupChunkDetail", () => {
  test("reads as a human part count", () => {
    expect(terminalStartupChunkDetail({ received: 3, total: 7 })).toBe("part 3 of 7");
  });

  test("unusable counts produce no line at all", () => {
    expect(terminalStartupChunkDetail(null)).toBeNull();
    expect(terminalStartupChunkDetail(undefined)).toBeNull();
    expect(terminalStartupChunkDetail({ received: 0, total: 0 })).toBeNull();
  });
});

describe("terminalStartupCompletesJourney", () => {
  test("only pane stages finish the journey; bootstrap hands off silently", () => {
    expect(["identity", "sync", "sessions"].map(
      (stage) => terminalStartupCompletesJourney(stage as TerminalStartupStage),
    )).toEqual([false, false, false]);
    expect(["spawn", "measure", "viewport", "frame", "render", "retry"].map(
      (stage) => terminalStartupCompletesJourney(stage as TerminalStartupStage),
    )).toEqual([true, true, true, true, true, true]);
  });
});
