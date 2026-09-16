// deriveTerminalPresentationState is the pure decision behind a pane's status
// dot: which of idle/receiving/catching_up/detached an operator is shown for a
// given pair of canonical and reconciled watermarks. These cases pin the
// window boundaries and the unready cases, and need no transport or replica —
// terminal lifecycle behavior is pinned in terminalStreamLifecycle.test.ts.

import { describe, expect, test } from "bun:test";
import {
  FRAME_ACTIVITY_WINDOW_MS,
  deriveTerminalPresentationState,
} from "../src/store/terminal-stream-types.ts";

describe("terminal stream presentation state", () => {
  const watermark = (grid_epoch: string, seq: number) => ({ grid_epoch, seq });

  test("reports receiving for recent equal canonical and reconciled watermarks, then idles", () => {
    const activity = {
      grid_epoch: "epoch-a",
      seq: 2,
      started_at_ms: 1_000,
    };
    expect(FRAME_ACTIVITY_WINDOW_MS).toBe(500);
    expect(deriveTerminalPresentationState({
      active: true,
      acceptedWithBaseline: true,
      canonical: watermark("epoch-a", 2),
      reconciled: watermark("epoch-a", 2),
      activity,
      nowMs: 1_499,
      notReadySinceMs: null,
    })).toBe("receiving");
    expect(deriveTerminalPresentationState({
      active: true,
      acceptedWithBaseline: true,
      canonical: watermark("epoch-a", 2),
      reconciled: watermark("epoch-a", 2),
      activity,
      nowMs: 1_500,
      notReadySinceMs: null,
    })).toBe("idle");
  });

  test("reports catching_up while canonical is ahead of the renderer", () => {
    expect(deriveTerminalPresentationState({
      active: true,
      acceptedWithBaseline: true,
      canonical: watermark("epoch-a", 3),
      reconciled: watermark("epoch-a", 2),
      activity: {
        grid_epoch: "epoch-a",
        seq: 3,
        started_at_ms: 1_000,
      },
      nowMs: 1_100,
      notReadySinceMs: null,
    })).toBe("catching_up");
  });

  test("returns to receiving after hold reconciliation, then expires to idle", () => {
    const activity = {
      grid_epoch: "epoch-a",
      seq: 4,
      started_at_ms: 2_000,
    };
    const input = {
      active: true,
      acceptedWithBaseline: true,
      canonical: watermark("epoch-a", 4),
      reconciled: watermark("epoch-a", 4),
      activity,
      notReadySinceMs: null,
    };
    expect(deriveTerminalPresentationState({ ...input, nowMs: 2_250 })).toBe("receiving");
    expect(deriveTerminalPresentationState({ ...input, nowMs: 2_500 })).toBe("idle");
  });

  test("keeps missing baseline and inactive panes idle even when watermarks differ", () => {
    const input = {
      acceptedWithBaseline: false,
      canonical: watermark("epoch-a", 3),
      reconciled: watermark("epoch-a", 2),
      activity: null,
      nowMs: 10_000,
      notReadySinceMs: null,
    };
    expect(deriveTerminalPresentationState({ ...input, active: true })).toBe("idle");
    expect(deriveTerminalPresentationState({
      ...input,
      active: false,
      acceptedWithBaseline: true,
    })).toBe("idle");
  });
});
