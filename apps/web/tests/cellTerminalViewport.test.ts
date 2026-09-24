// Viewport publication timing for one mounted cell terminal.
// These tests pin resize coalescing, park transitions, and unmeasured-box retry
// policy on the module that owns them, with no lifecycle mounted.
// Font-metric invalidation is pinned in cellTerminalLifecycle.fonts.test.ts.

import { afterEach, beforeEach, describe, expect, mock, test, vi } from "bun:test";
import {
  createCellTerminalViewport,
  type CellTerminalViewport,
} from "../src/components/terminal/cell-terminal-viewport.ts";

interface FakeDisplay {
  clientWidth: number;
  clientHeight: number;
}

interface ViewportFixture {
  display: FakeDisplay;
  published: Array<{ cols: number; rows: number }>;
  setViewActive(active: boolean): void;
  viewport: CellTerminalViewport;
}

const browserGlobals = globalThis as unknown as Record<string, unknown>;
let savedGlobals: Record<string, unknown>;
let animationFrameCallbacks: Map<number, () => void>;
let nextAnimationFrameHandle: number;

mock.module("@roost/observability/diag", () => ({ diag: () => undefined }));
mock.module("../src/browser/pageVisible.ts", () => ({ isPageVisible: () => true }));

function createViewportFixture(): ViewportFixture {
  const display: FakeDisplay = { clientWidth: 800, clientHeight: 400 };
  const published: Array<{ cols: number; rows: number }> = [];
  let viewActive = true;
  const runtime = {
    sessionId: "viewport-session",
    unmounted: false,
    display: () => display as unknown as HTMLDivElement,
    cellWidth: 10,
    cellHeight: 20,
    view: {
      viewId: "viewport-view",
      setInactive: () => undefined,
      setViewport: (geometry: { cols: number; rows: number }) => published.push(geometry),
    },
    backfill: null,
  };
  const presentation = {
    clearFrameActivity: () => undefined,
    clearCursorBlink: () => undefined,
    releasePaintHolds: () => undefined,
  };
  const viewport = createCellTerminalViewport(
    runtime as never,
    presentation as never,
    () => false,
    () => viewActive,
  );
  return {
    display,
    published,
    setViewActive: (active) => {
      viewActive = active;
    },
    viewport,
  };
}

function flushAnimationFrames(): void {
  const callbacks = [...animationFrameCallbacks.values()];
  animationFrameCallbacks.clear();
  for (const callback of callbacks) callback();
}

beforeEach(() => {
  vi.useFakeTimers();
  animationFrameCallbacks = new Map();
  nextAnimationFrameHandle = 1;
  savedGlobals = {
    getComputedStyle: browserGlobals.getComputedStyle,
    requestAnimationFrame: browserGlobals.requestAnimationFrame,
    cancelAnimationFrame: browserGlobals.cancelAnimationFrame,
  };
  browserGlobals.getComputedStyle = () => ({
    paddingLeft: "0px",
    paddingRight: "0px",
    paddingTop: "0px",
    paddingBottom: "0px",
  });
  browserGlobals.requestAnimationFrame = (callback: () => void): number => {
    const handle = nextAnimationFrameHandle++;
    animationFrameCallbacks.set(handle, callback);
    return handle;
  };
  browserGlobals.cancelAnimationFrame = (handle: number): void => {
    animationFrameCallbacks.delete(handle);
  };
});

afterEach(() => {
  vi.useRealTimers();
  for (const [key, value] of Object.entries(savedGlobals)) browserGlobals[key] = value;
});

describe("cell terminal viewport publication", () => {
  test("publishes the latest settled geometry once after a 50 ms trailing resize", () => {
    const fixture = createViewportFixture();

    fixture.viewport.scheduleViewport();
    vi.advanceTimersByTime(25);
    fixture.display.clientWidth = 1_000;
    fixture.display.clientHeight = 500;
    fixture.viewport.scheduleViewport();

    vi.advanceTimersByTime(49);
    expect(fixture.published).toEqual([]);
    vi.advanceTimersByTime(1);
    expect(fixture.published).toEqual([{ cols: 100, rows: 25 }]);
    vi.advanceTimersByTime(500);
    expect(fixture.published).toEqual([{ cols: 100, rows: 25 }]);
  });

  test("immediate activation or recovery publication cancels a pending trailing resize", () => {
    const fixture = createViewportFixture();

    fixture.viewport.scheduleViewport();
    fixture.display.clientWidth = 900;
    fixture.display.clientHeight = 440;
    expect(fixture.viewport.publishViewportNow()).toBe(true);
    expect(fixture.published).toEqual([{ cols: 90, rows: 22 }]);
    vi.advanceTimersByTime(100);
    expect(fixture.published).toEqual([{ cols: 90, rows: 22 }]);
  });

  test("publishes once when the 100 ms retry finds geometry after two failures", () => {
    const fixture = createViewportFixture();
    fixture.display.clientWidth = 0;

    expect(fixture.viewport.publishViewportNow()).toBe(false);
    expect(animationFrameCallbacks.size).toBe(1);
    flushAnimationFrames();
    vi.advanceTimersByTime(99);
    expect(fixture.published).toEqual([]);

    fixture.display.clientWidth = 800;
    vi.advanceTimersByTime(1);
    expect(fixture.published).toEqual([{ cols: 80, rows: 20 }]);
    vi.advanceTimersByTime(500);
    expect(fixture.published).toEqual([{ cols: 80, rows: 20 }]);
  });

  test("cancels parked and inactive retries before a later activation", () => {
    const fixture = createViewportFixture();
    fixture.display.clientWidth = 0;

    expect(fixture.viewport.publishViewportNow()).toBe(false);
    fixture.viewport.parkView();
    fixture.display.clientWidth = 800;
    flushAnimationFrames();
    expect(fixture.published).toEqual([]);

    fixture.display.clientWidth = 0;
    expect(fixture.viewport.publishViewportNow()).toBe(false);
    flushAnimationFrames();
    fixture.setViewActive(false);
    fixture.viewport.publishInactive();
    fixture.display.clientWidth = 800;
    vi.advanceTimersByTime(100);
    expect(fixture.published).toEqual([]);

    fixture.setViewActive(true);
    fixture.display.clientWidth = 0;
    expect(fixture.viewport.publishViewportNow()).toBe(false);
    fixture.display.clientWidth = 800;
    flushAnimationFrames();
    expect(fixture.published).toEqual([{ cols: 80, rows: 20 }]);
  });

  test("a park cancels the 100 ms retry an episode already escalated to", () => {
    const fixture = createViewportFixture();
    fixture.display.clientWidth = 0;

    expect(fixture.viewport.publishViewportNow()).toBe(false);
    flushAnimationFrames();
    fixture.viewport.parkView();

    fixture.display.clientWidth = 800;
    vi.advanceTimersByTime(100);
    expect(fixture.published).toEqual([]);
  });

  test("coalesces a positive retry with a pending trailing resize", () => {
    const fixture = createViewportFixture();
    fixture.display.clientWidth = 0;

    expect(fixture.viewport.publishViewportNow()).toBe(false);
    flushAnimationFrames();
    vi.advanceTimersByTime(50);
    fixture.viewport.scheduleViewport();
    fixture.display.clientWidth = 800;
    vi.advanceTimersByTime(50);

    expect(fixture.published).toEqual([{ cols: 80, rows: 20 }]);
    vi.advanceTimersByTime(100);
    expect(fixture.published).toEqual([{ cols: 80, rows: 20 }]);
  });

  test("starts a new retry episode only after a later publication request", () => {
    const fixture = createViewportFixture();
    fixture.display.clientWidth = 0;

    expect(fixture.viewport.publishViewportNow()).toBe(false);
    flushAnimationFrames();
    vi.advanceTimersByTime(100);
    expect(animationFrameCallbacks.size).toBe(0);

    expect(fixture.viewport.publishViewportNow()).toBe(false);
    expect(animationFrameCallbacks.size).toBe(1);
    fixture.display.clientWidth = 800;
    flushAnimationFrames();
    expect(fixture.published).toEqual([{ cols: 80, rows: 20 }]);
  });
});
