import { afterEach, describe, expect, test, vi } from "bun:test";
import { createRoot, createSignal } from "solid-js";
import type { CellGridRenderer } from "../src/lib/cellRenderer.ts";
import {
  createTerminalPresentationController,
  FOREGROUND_DOM_STALL_MS,
  preservesForegroundReaderHold,
} from "../src/lib/terminalPresentation.ts";
import type { TerminalViewHandleStatus } from "../src/store/terminal-stream-types.ts";

const accepted: TerminalViewHandleStatus = {
  status: "accepted",
  revision: 1n,
  active: true,
  streamId: "10000000-0000-4000-8000-000000000001",
  effectiveCols: 80,
  effectiveRows: 24,
  baselineReady: true,
};

afterEach(() => {
  vi.useRealTimers();
});

describe("foreground terminal presentation stalls", () => {
  test("fires at the oldest unreconciled watermark without resetting on newer frames", () => {
    vi.useFakeTimers();
    const [active] = createSignal(true);
    let canonical = { grid_epoch: "epoch-a", seq: 1 as number | null };
    let reconciled = { grid_epoch: "epoch-a", seq: 0 as number | null };
    const renderer = {
      canonicalEpochSeq: () => ({ ...canonical }),
      reconciledEpochSeq: () => ({ ...reconciled }),
      setCursorBlinkEnabled: () => undefined,
    } as unknown as CellGridRenderer;
    const stalled: Array<{ grid_epoch: string | null; seq: number | null }> = [];
    let dispose: () => void = () => undefined;
    const controller = createRoot((rootDispose) => {
      dispose = rootDispose;
      return createTerminalPresentationController({
        active,
        focused: () => true,
        status: () => accepted,
        renderer: () => renderer,
        onCatchUpStalled: (watermark) => stalled.push({ ...watermark }),
      });
    });
    try {
      controller.refreshTerminalPresentation();
      expect(controller.state()).toBe("catching_up");
      vi.advanceTimersByTime(FOREGROUND_DOM_STALL_MS / 2);
      canonical = { grid_epoch: "epoch-a", seq: 2 };
      controller.refreshTerminalPresentation();
      vi.advanceTimersByTime(FOREGROUND_DOM_STALL_MS / 2 - 1);
      expect(stalled).toEqual([]);
      vi.advanceTimersByTime(1);
      expect(stalled).toEqual([{ grid_epoch: "epoch-a", seq: 1 }]);

      controller.refreshTerminalPresentation();
      vi.advanceTimersByTime(FOREGROUND_DOM_STALL_MS);
      expect(stalled).toEqual([
        { grid_epoch: "epoch-a", seq: 1 },
        { grid_epoch: "epoch-a", seq: 2 },
      ]);

      reconciled = { grid_epoch: "epoch-a", seq: 2 };
      controller.refreshTerminalPresentation();
      expect(controller.state()).toBe("idle");
    } finally {
      dispose();
    }
  });

  test("inactive panes cancel a pending catch-up callback", () => {
    vi.useFakeTimers();
    const [active, setActive] = createSignal(true);
    const renderer = {
      canonicalEpochSeq: () => ({ grid_epoch: "epoch-a", seq: 2 }),
      reconciledEpochSeq: () => ({ grid_epoch: "epoch-a", seq: 1 }),
      setCursorBlinkEnabled: () => undefined,
    } as unknown as CellGridRenderer;
    let calls = 0;
    let dispose: () => void = () => undefined;
    const controller = createRoot((rootDispose) => {
      dispose = rootDispose;
      return createTerminalPresentationController({
        active,
        focused: () => true,
        status: () => accepted,
        renderer: () => renderer,
        onCatchUpStalled: () => { calls++; },
      });
    });
    try {
      controller.refreshTerminalPresentation();
      setActive(false);
      controller.refreshTerminalPresentation();
      vi.advanceTimersByTime(FOREGROUND_DOM_STALL_MS);
      expect(calls).toBe(0);
      expect(controller.state()).toBe("idle");
    } finally {
      dispose();
    }
  });

  test("a long-lived selection defers recovery without consuming the same-generation stall", () => {
    vi.useFakeTimers();
    let readerReason: "selection" | null = "selection";
    const renderer = {
      canonicalEpochSeq: () => ({ grid_epoch: "epoch-a", seq: 2 }),
      reconciledEpochSeq: () => ({ grid_epoch: "epoch-a", seq: 1 }),
      get readerReason() {
        return readerReason;
      },
      setCursorBlinkEnabled: () => undefined,
    } as unknown as CellGridRenderer;
    let calls = 0;
    let dispose: () => void = () => undefined;
    const controller = createRoot((rootDispose) => {
      dispose = rootDispose;
      return createTerminalPresentationController({
        active: () => true,
        focused: () => true,
        status: () => accepted,
        renderer: () => renderer,
        onCatchUpStalled: () => { calls++; },
      });
    });
    try {
      controller.refreshTerminalPresentation();
      vi.advanceTimersByTime(FOREGROUND_DOM_STALL_MS * 2);
      expect(calls).toBe(0);
      expect(controller.state()).toBe("catching_up");

      readerReason = null;
      controller.refreshTerminalPresentation();
      vi.advanceTimersByTime(FOREGROUND_DOM_STALL_MS - 1);
      expect(calls).toBe(0);
      vi.advanceTimersByTime(1);
      expect(calls).toBe(1);
    } finally {
      dispose();
    }
  });

  test("preserves every explicit reader hold", () => {
    expect(preservesForegroundReaderHold("native_scroll")).toBe(true);
    expect(preservesForegroundReaderHold("wheel")).toBe(true);
    expect(preservesForegroundReaderHold("touch")).toBe(true);
    expect(preservesForegroundReaderHold("selection")).toBe(true);
    expect(preservesForegroundReaderHold("find")).toBe(true);
    expect(preservesForegroundReaderHold(null)).toBe(false);
  });
});
