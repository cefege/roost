import { afterEach, describe, expect, test, vi } from "bun:test";
import { createRoot, createSignal } from "solid-js";
import type { CellGridRenderer } from "../../src/renderer/cellRenderer.ts";
import {
  createTerminalPresentationController,
  FOREGROUND_DOM_STALL_MS,
  preservesForegroundReaderHold,
} from "../../src/renderer/terminalPresentation.ts";
import { setForceHidden } from "../../src/browser/pageVisible.ts";
import {
  DETACHED_GRACE_MS,
  type TerminalViewHandleStatus,
} from "../../src/store/terminal-stream-types.ts";

const accepted: TerminalViewHandleStatus = {
  status: "accepted",
  revision: 1n,
  active: true,
  streamId: "10000000-0000-4000-8000-000000000001",
  effectiveCols: 80,
  effectiveRows: 24,
  baselineReady: true,
};

/** The production freeze shape: the view is accepted and active, but its
 *  baseline never arrived, so there is nothing live to paint. */
const acceptedWithoutBaseline: TerminalViewHandleStatus = { ...accepted, baselineReady: false };

/** Renderer stub for the presentation paths that only read watermarks; its
 *  canonical and reconciled watermarks are always equal, so a ready view never
 *  lands in catching_up. */
const reconciledRenderer = {
  canonicalEpochSeq: () => ({ grid_epoch: "epoch-a", seq: 5 }),
  reconciledEpochSeq: () => ({ grid_epoch: "epoch-a", seq: 5 }),
  setCursorBlinkEnabled: () => undefined,
} as unknown as CellGridRenderer;

afterEach(() => {
  vi.useRealTimers();
  setForceHidden(false);
});

describe("foreground terminal presentation stalls", () => {
  test("uses a one-second foreground DOM stall deadline", () => {
    expect(FOREGROUND_DOM_STALL_MS).toBe(1_000);
  });

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

describe("detached terminal panes", () => {
  test("an actively viewed pane without an accepted view detaches once the grace expires", () => {
    vi.useFakeTimers();
    let dispose: () => void = () => undefined;
    const controller = createRoot((rootDispose) => {
      dispose = rootDispose;
      return createTerminalPresentationController({
        active: () => true,
        focused: () => true,
        status: () => acceptedWithoutBaseline,
        renderer: () => reconciledRenderer,
        onCatchUpStalled: () => undefined,
      });
    });
    try {
      controller.refreshTerminalPresentation();
      expect(controller.state()).toBe("idle");
      vi.advanceTimersByTime(DETACHED_GRACE_MS - 1);
      expect(controller.state()).toBe("idle");
      vi.advanceTimersByTime(1);
      expect(controller.state()).toBe("detached");
    } finally {
      dispose();
    }
  });

  test("the grace runs from the first lost view, not from the latest refresh", () => {
    vi.useFakeTimers();
    let dispose: () => void = () => undefined;
    const controller = createRoot((rootDispose) => {
      dispose = rootDispose;
      return createTerminalPresentationController({
        active: () => true,
        focused: () => true,
        status: () => acceptedWithoutBaseline,
        renderer: () => reconciledRenderer,
        onCatchUpStalled: () => undefined,
      });
    });
    try {
      controller.refreshTerminalPresentation();
      for (let elapsedMs = 0; elapsedMs < DETACHED_GRACE_MS; elapsedMs += 100) {
        vi.advanceTimersByTime(100);
        controller.refreshTerminalPresentation();
      }
      expect(controller.state()).toBe("detached");
    } finally {
      dispose();
    }
  });

  test("a view that becomes ready inside the grace never detaches", () => {
    vi.useFakeTimers();
    let status: TerminalViewHandleStatus = acceptedWithoutBaseline;
    let dispose: () => void = () => undefined;
    const controller = createRoot((rootDispose) => {
      dispose = rootDispose;
      return createTerminalPresentationController({
        active: () => true,
        focused: () => true,
        status: () => status,
        renderer: () => reconciledRenderer,
        onCatchUpStalled: () => undefined,
      });
    });
    try {
      controller.refreshTerminalPresentation();
      vi.advanceTimersByTime(DETACHED_GRACE_MS - 1);
      status = accepted;
      controller.refreshTerminalPresentation();
      expect(controller.state()).toBe("idle");
      vi.advanceTimersByTime(DETACHED_GRACE_MS * 3);
      expect(controller.state()).toBe("idle");
    } finally {
      dispose();
    }
  });

  test("panes that are not actively viewed never detach", () => {
    vi.useFakeTimers();
    let disposeInactive: () => void = () => undefined;
    const inactive = createRoot((rootDispose) => {
      disposeInactive = rootDispose;
      return createTerminalPresentationController({
        active: () => false,
        focused: () => true,
        status: () => acceptedWithoutBaseline,
        renderer: () => reconciledRenderer,
        onCatchUpStalled: () => undefined,
      });
    });
    setForceHidden(true);
    let disposeHidden: () => void = () => undefined;
    const hidden = createRoot((rootDispose) => {
      disposeHidden = rootDispose;
      return createTerminalPresentationController({
        active: () => true,
        focused: () => true,
        status: () => acceptedWithoutBaseline,
        renderer: () => reconciledRenderer,
        onCatchUpStalled: () => undefined,
      });
    });
    try {
      inactive.refreshTerminalPresentation();
      hidden.refreshTerminalPresentation();
      vi.advanceTimersByTime(DETACHED_GRACE_MS * 10);
      expect(inactive.state()).toBe("idle");
      expect(hidden.state()).toBe("idle");
    } finally {
      disposeInactive();
      disposeHidden();
    }
  });

  test("a detached pane reports receiving again on the next accepted delta", () => {
    vi.useFakeTimers();
    let status: TerminalViewHandleStatus = acceptedWithoutBaseline;
    let dispose: () => void = () => undefined;
    const controller = createRoot((rootDispose) => {
      dispose = rootDispose;
      return createTerminalPresentationController({
        active: () => true,
        focused: () => true,
        status: () => status,
        renderer: () => reconciledRenderer,
        onCatchUpStalled: () => undefined,
      });
    });
    try {
      controller.refreshTerminalPresentation();
      vi.advanceTimersByTime(DETACHED_GRACE_MS);
      expect(controller.state()).toBe("detached");
      status = accepted;
      controller.noteFrameActivity({ full: false, gridEpoch: "epoch-a", seq: 5 });
      expect(controller.state()).toBe("receiving");
    } finally {
      dispose();
    }
  });
});
