// Viewport publication timing for one mounted cell terminal.
// These tests keep resize coalescing separate from lifecycle-owned activation,
// drag suppression, and unmeasured-box retry policy.

import { afterEach, beforeEach, describe, expect, mock, test, vi } from "bun:test";
import type * as SolidApi from "solid-js";
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
  setViewActive(active: boolean): void;
  viewport: CellTerminalViewport;
}

interface LifecycleFixture {
  readonly events: string[];
  readonly runtime: { cellHeight: number; cellWidth: number };
  dispose(): void;
}

class FakeResizeObserver {
  disconnect(): void {}
  observe(): void {}
}

class FakeLifecycleDocument extends EventTarget {
  activeElement: Element | null = null;
  body = {} as HTMLElement;
  documentElement = {} as HTMLElement;
  fonts: { ready: Promise<void> };
  visibilityState = "visible" as DocumentVisibilityState;

  constructor(fontReady: Promise<void>) {
    super();
    this.fonts = { ready: fontReady };
  }

  hasFocus(): boolean {
    return true;
  }
}

const browserGlobals = globalThis as unknown as Record<string, unknown>;
let savedGlobals: Record<string, unknown>;
let animationFrameCallbacks: Map<number, () => void>;
let nextAnimationFrameHandle: number;
let lifecycleVisible = true;

const lifecycleBootstrapDocument = new FakeLifecycleDocument(Promise.resolve());
browserGlobals.ResizeObserver = FakeResizeObserver;
browserGlobals.document = lifecycleBootstrapDocument;
browserGlobals.window = new EventTarget();
browserGlobals.requestAnimationFrame = () => 1;
browserGlobals.cancelAnimationFrame = () => undefined;

// The client runtime must load after fake browser globals; Bun resolves public solid-js to SSR.
const solidClientUrl = new URL("./solid.js", import.meta.resolve("solid-js"));
const Solid = await import(solidClientUrl.href) as typeof SolidApi;
mock.module("solid-js", () => ({ ...Solid }));
mock.module("@roost/shared/diag", () => ({ diag: () => undefined }));
mock.module("../src/lib/focusOwners.ts", () => ({
  FOCUS_OWNERS: "[data-focus-owner]",
}));
mock.module("../src/lib/pageVisible.ts", () => ({
  isPageVisible: () => lifecycleVisible,
}));
mock.module("../src/lib/resizeDrag.ts", () => ({
  arrangeEpoch: () => 0,
  isResizeDragging: () => false,
}));
mock.module("../src/lib/terminalFontPref.ts", () => ({ termFontSize: () => 14 }));
mock.module("../src/lib/terminalInput.ts", () => ({ isAltGraphKey: () => false }));
mock.module("../src/lib/windowSizeClass.ts", () => ({ isTouchDevice: () => false }));

// Lifecycle imports after its Solid and browser-adapter mocks bind.
const { mountCellTerminalLifecycle } = await import(
  "../src/components/cell-terminal-lifecycle.ts"
);

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

function mountLifecycleFixture(onPark?: () => void): LifecycleFixture {
  const [viewActive] = Solid.createSignal(true);
  const events: string[] = [];
  const runtime = {
    cellHeight: 16,
    cellWidth: 8,
    display: () => ({} as HTMLDivElement),
    inputController: null,
    renderer: null,
    revealStartedAt: 0,
    sessionId: "lifecycle-session",
    unmounted: false,
    view: null,
  };
  const presentation = {
    clearCursorBlink: () => undefined,
    clearFrameActivity: () => undefined,
    notifyBackfill: () => undefined,
    refreshCursorBlink: () => undefined,
    refreshTerminalPresentation: () => undefined,
    releasePaintHolds: () => undefined,
  };
  const viewport = {
    cancelScheduled: () => undefined,
    parkView: () => {
      events.push("park-view");
      onPark?.();
    },
    publishInactive: () => undefined,
    publishViewport: () => false,
    publishViewportNow: () => {
      events.push("publish-viewport-now");
      return true;
    },
    scheduleViewport: () => undefined,
    shouldPublishActive: () => lifecycleVisible && viewActive(),
    viewActive,
  };
  const input = {
    copySelectionToClipboard: async () => undefined,
    find: { openFind: () => undefined },
    pasteFromClipboard: async () => undefined,
  };
  let disposeRoot = (): void => undefined;
  let lifecycle: { dispose(): void } | null = null;
  Solid.createRoot((dispose) => {
    disposeRoot = dispose;
    lifecycle = mountCellTerminalLifecycle(
      {
        focused: true,
        session: { git_remote: null, id: "lifecycle-session" },
        spotlit: false,
      } as never,
      runtime as never,
      input as never,
      presentation as never,
      viewport as never,
      () => false,
    );
  });
  events.length = 0;
  let disposed = false;
  return {
    events,
    runtime,
    dispose: () => {
      if (disposed) return;
      disposed = true;
      lifecycle?.dispose();
      disposeRoot();
    },
  };
}

beforeEach(() => {
  vi.useFakeTimers();
  animationFrameCallbacks = new Map();
  nextAnimationFrameHandle = 1;
  savedGlobals = {
    document: browserGlobals.document,
    getComputedStyle: browserGlobals.getComputedStyle,
    requestAnimationFrame: browserGlobals.requestAnimationFrame,
    cancelAnimationFrame: browserGlobals.cancelAnimationFrame,
    window: browserGlobals.window,
    ResizeObserver: browserGlobals.ResizeObserver,
  };
  browserGlobals.document = { visibilityState: "visible" };
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
  lifecycleVisible = true;
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

describe("cell terminal lifecycle font readiness", () => {
  test("clears active cached dimensions and publishes when fonts become ready", async () => {
    const fontReady = Promise.withResolvers<void>();
    browserGlobals.document = new FakeLifecycleDocument(fontReady.promise);
    browserGlobals.window = new EventTarget();
    browserGlobals.ResizeObserver = FakeResizeObserver;
    const fixture = mountLifecycleFixture();

    fontReady.resolve();
    await Promise.resolve();

    expect(fixture.runtime.cellWidth).toBe(0);
    expect(fixture.runtime.cellHeight).toBe(0);
    expect(fixture.events).toEqual(["publish-viewport-now"]);
    fixture.dispose();
  });

  test("cancels a pending retry and ignores font readiness after disposal", async () => {
    const fontReady = Promise.withResolvers<void>();
    browserGlobals.document = new FakeLifecycleDocument(fontReady.promise);
    browserGlobals.window = new EventTarget();
    browserGlobals.ResizeObserver = FakeResizeObserver;
    const retryFixture = createViewportFixture();
    retryFixture.display.clientWidth = 0;
    expect(retryFixture.viewport.publishViewportNow()).toBe(false);
    flushAnimationFrames();
    const fixture = mountLifecycleFixture(() => retryFixture.viewport.parkView());

    fixture.dispose();
    expect(fixture.events).toEqual(["park-view"]);
    retryFixture.display.clientWidth = 800;
    vi.advanceTimersByTime(100);
    fontReady.resolve();
    await Promise.resolve();

    expect(retryFixture.published).toEqual([]);
    expect(fixture.runtime.cellWidth).toBe(8);
    expect(fixture.runtime.cellHeight).toBe(16);
    expect(fixture.events).toEqual(["park-view"]);
  });
});
