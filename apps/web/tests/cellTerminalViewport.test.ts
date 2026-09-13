// Viewport publication timing for one mounted cell terminal.
// These tests keep resize coalescing separate from lifecycle-owned activation,
// drag suppression, and unmeasured-box retry policy.

import { afterEach, beforeEach, describe, expect, test, vi } from "bun:test";
import {
  createCellTerminalViewport,
  type CellTerminalViewport,
} from "../src/components/cell-terminal-viewport.ts";

interface FakeDisplay {
  clientWidth: number;
  clientHeight: number;
}

interface ViewportFixture {
  display: FakeDisplay;
  published: Array<{ cols: number; rows: number }>;
  viewport: CellTerminalViewport;
}

const browserGlobals = globalThis as unknown as Record<string, unknown>;
let savedGlobals: Record<string, unknown>;
let animationFrames: Array<(() => void) | null>;

function createViewportFixture(): ViewportFixture {
  const display: FakeDisplay = { clientWidth: 800, clientHeight: 400 };
  const published: Array<{ cols: number; rows: number }> = [];
  const runtime = {
    sessionId: "viewport-session",
    unmounted: false,
    display: () => display as unknown as HTMLDivElement,
    cellWidth: 10,
    cellHeight: 20,
    view: {
      viewId: "viewport-view",
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
    () => true,
  );
  return { display, published, viewport };
}

beforeEach(() => {
  vi.useFakeTimers();
  animationFrames = [];
  savedGlobals = {
    document: browserGlobals.document,
    getComputedStyle: browserGlobals.getComputedStyle,
    requestAnimationFrame: browserGlobals.requestAnimationFrame,
    cancelAnimationFrame: browserGlobals.cancelAnimationFrame,
  };
  browserGlobals.document = { visibilityState: "visible" };
  browserGlobals.getComputedStyle = () => ({
    paddingLeft: "0px",
    paddingRight: "0px",
    paddingTop: "0px",
    paddingBottom: "0px",
  });
  browserGlobals.requestAnimationFrame = (callback: () => void): number => {
    animationFrames.push(callback);
    return animationFrames.length;
  };
  browserGlobals.cancelAnimationFrame = (handle: number): void => {
    animationFrames[handle - 1] = null;
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

  test("an unmeasured active box retries on the next animation frame", () => {
    const fixture = createViewportFixture();
    fixture.display.clientWidth = 0;

    expect(fixture.viewport.publishViewportNow()).toBe(false);
    expect(animationFrames).toHaveLength(1);
    fixture.display.clientWidth = 800;
    const retry = animationFrames.shift();
    retry?.();

    expect(fixture.published).toEqual([{ cols: 80, rows: 20 }]);
  });
});
