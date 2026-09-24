// Font-metric invalidation for one mounted cell terminal, observed end to end.
// Composes the real viewport publisher with the real lifecycle, so a webfont
// that settles behind a hidden, inactive, or pending pane is judged only by the
// geometry that pane claims next. Owns the fake FontFaceSet, the probe span the
// real measurement re-reads, and the display box; publication timing itself
// belongs to cellTerminalViewport.test.ts.

import { afterEach, beforeEach, describe, expect, mock, test, vi } from "bun:test";
import type * as SolidApi from "solid-js";
import type { TerminalGeometry } from "@roost/protocol/viewport";

const DISPLAY_WIDTH = 320;
const DISPLAY_HEIGHT = 160;
// CELL_PROBE_TEXT in ../src/lib/terminalCellGeometry.ts spans ten cells.
const PROBE_CELL_COUNT = 10;
// The publisher debounces a claim at 50 ms and retries an unmeasured box at
// 100 ms; one advance past both settles everything a transition scheduled.
const PUBLICATION_SETTLE_MS = 150;

interface ProbeCellMetrics {
  width: number;
  height: number;
}

interface TerminalFontsFixture {
  dispatchFontLoadingEvent(type: "loadingdone" | "loadingerror"): void;
  dispose(): void;
  setPending(pending: boolean): void;
  setProbeCell(width: number, height: number): void;
  setViewActive(active: boolean): void;
  settleFontsReady(): void;
  takeClaims(): TerminalGeometry[];
}

class FakeFontFaceSet extends EventTarget {
  readonly ready: Promise<void>;

  constructor(ready: Promise<void>) {
    super();
    this.ready = ready;
  }
}

class FakeTerminalDocument extends EventTarget {
  activeElement: Element | null = null;
  body = {} as HTMLElement;
  documentElement = {} as HTMLElement;
  visibilityState = "visible" as DocumentVisibilityState;

  constructor(
    readonly fonts: FakeFontFaceSet,
    private readonly probeCell: ProbeCellMetrics,
  ) {
    super();
  }

  // measureTerminalCellBox parents this probe in the grid and divides its rect
  // by the probe length, so probeCell is the advance the font actually paints.
  createElement(): unknown {
    const probeCell = this.probeCell;
    return {
      className: "",
      style: {} as Record<string, string>,
      textContent: "",
      getBoundingClientRect: () => ({
        width: probeCell.width * PROBE_CELL_COUNT,
        height: probeCell.height,
      }),
    };
  }

  hasFocus(): boolean {
    return true;
  }

  querySelector(): null {
    return null;
  }
}

class FakeResizeObserver {
  disconnect(): void {}
  observe(): void {}
}

const browserGlobals = globalThis as unknown as Record<string, unknown>;
let savedGlobals: Record<string, unknown>;
let animationFrameCallbacks: Map<number, () => void>;
let nextAnimationFrameHandle: number;
let terminalPageVisible = true;

browserGlobals.ResizeObserver = FakeResizeObserver;
browserGlobals.document = new FakeTerminalDocument(
  new FakeFontFaceSet(Promise.resolve()),
  { width: 8, height: 16 },
);
browserGlobals.window = new EventTarget();
browserGlobals.requestAnimationFrame = () => 1;
browserGlobals.cancelAnimationFrame = () => undefined;

// The client runtime must load after fake browser globals; Bun resolves public solid-js to SSR.
const solidClientUrl = new URL("./solid.js", import.meta.resolve("solid-js"));
const Solid = await import(solidClientUrl.href) as typeof SolidApi;
mock.module("solid-js", () => ({ ...Solid }));
mock.module("@roost/observability/diag", () => ({ diag: () => undefined }));
mock.module("../src/lib/focusOwners.ts", () => ({
  FOCUS_OWNERS: "[data-focus-owner]",
}));
mock.module("../src/browser/pageVisible.ts", () => ({
  isPageVisible: () => terminalPageVisible,
}));
mock.module("../src/lib/resizeDrag.ts", () => ({
  arrangeEpoch: () => 0,
  isResizeDragging: () => false,
}));
mock.module("../src/store/prefs/terminalFontPref.ts", () => ({ termFontSize: () => 14 }));
mock.module("../src/client/input/terminalInput.ts", () => ({ isAltGraphKey: () => false }));
mock.module("../src/browser/windowSizeClass.ts", () => ({ isTouchDevice: () => false }));

// Viewport and lifecycle import after their Solid and browser-adapter mocks bind.
const { createCellTerminalViewport } = await import(
  "../src/components/terminal/cell-terminal-viewport.ts"
);
const { mountCellTerminalLifecycle } = await import(
  "../src/components/terminal/cell-terminal-lifecycle.ts"
);

function mountTerminalFontsFixture(): TerminalFontsFixture {
  const fontsReady = Promise.withResolvers<void>();
  const fonts = new FakeFontFaceSet(fontsReady.promise);
  const probeCell: ProbeCellMetrics = { width: 8, height: 16 };
  browserGlobals.document = new FakeTerminalDocument(fonts, probeCell);
  browserGlobals.window = new EventTarget();
  const display = {
    appendChild: () => undefined,
    clientHeight: DISPLAY_HEIGHT,
    clientWidth: DISPLAY_WIDTH,
    removeChild: () => undefined,
  };
  const claims: TerminalGeometry[] = [];
  const [viewActive, setViewActive] = Solid.createSignal(true);
  const [pending, setPending] = Solid.createSignal(false);
  const runtime = {
    backfill: null,
    cellHeight: 0,
    cellWidth: 0,
    display: () => display as unknown as HTMLDivElement,
    inputController: null,
    renderer: null,
    revealStartedAt: 0,
    sessionId: "fonts-session",
    unmounted: false,
    view: {
      refresh: () => undefined,
      setInactive: () => undefined,
      setViewport: (geometry: TerminalGeometry) => claims.push(geometry),
      viewId: "fonts-view",
    },
  };
  const presentation = {
    clearCursorBlink: () => undefined,
    clearFrameActivity: () => undefined,
    notifyBackfill: () => undefined,
    refreshCursorBlink: () => undefined,
    refreshTerminalPresentation: () => undefined,
    releasePaintHolds: () => undefined,
  };
  const input = {
    copySelectionToClipboard: async () => undefined,
    find: { openFind: () => undefined },
    pasteFromClipboard: async () => undefined,
  };
  const viewport = createCellTerminalViewport(
    runtime as never,
    presentation as never,
    pending,
    viewActive,
  );
  let disposeRoot = (): void => undefined;
  let lifecycle: { dispose(): void } | null = null;
  Solid.createRoot((dispose) => {
    disposeRoot = dispose;
    lifecycle = mountCellTerminalLifecycle(
      {
        focused: true,
        session: { git_remote: null, id: "fonts-session" },
        spotlit: false,
        surfaceActive: true,
        surfaceVisible: true,
      } as never,
      runtime as never,
      input as never,
      presentation as never,
      viewport,
      pending,
    );
  });
  let disposed = false;
  return {
    dispatchFontLoadingEvent: (type) => {
      fonts.dispatchEvent(new Event(type));
    },
    dispose: () => {
      if (disposed) return;
      disposed = true;
      lifecycle?.dispose();
      disposeRoot();
    },
    setPending: (next) => {
      setPending(next);
    },
    setProbeCell: (width, height) => {
      probeCell.width = width;
      probeCell.height = height;
    },
    setViewActive: (active) => {
      setViewActive(active);
    },
    settleFontsReady: () => fontsReady.resolve(),
    takeClaims: () => claims.splice(0, claims.length),
  };
}

function flushAnimationFrames(): void {
  const callbacks = [...animationFrameCallbacks.values()];
  animationFrameCallbacks.clear();
  for (const callback of callbacks) callback();
}

function settlePublications(): void {
  flushAnimationFrames();
  vi.advanceTimersByTime(PUBLICATION_SETTLE_MS);
  flushAnimationFrames();
}

/** Font completion arrives as a promise reaction; yielding the microtask queue
 *  twice runs it and anything it chains before the next assertion. */
async function flushFontCompletion(): Promise<void> {
  await Promise.resolve();
  await Promise.resolve();
}

/** Republishing the geometry a pane already claims is publisher timing, pinned
 *  in cellTerminalViewport.test.ts. These tests read WHICH geometries a pane
 *  claims, so a repeat of the current claim folds into it while an empty drain
 *  still means the pane claimed nothing. */
function claimSequence(claims: readonly TerminalGeometry[]): TerminalGeometry[] {
  const sequence: TerminalGeometry[] = [];
  for (const claim of claims) {
    const last = sequence.at(-1);
    if (last?.cols === claim.cols && last?.rows === claim.rows) continue;
    sequence.push(claim);
  }
  return sequence;
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
  browserGlobals.ResizeObserver = FakeResizeObserver;
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
  terminalPageVisible = true;
  for (const [key, value] of Object.entries(savedGlobals)) browserGlobals[key] = value;
});

describe("cell terminal font metric invalidation", () => {
  test("claims the box fitted to the advance measured at mount", () => {
    const fixture = mountTerminalFontsFixture();

    settlePublications();

    expect(claimSequence(fixture.takeClaims())).toEqual([{ cols: 40, rows: 10 }]);
    fixture.dispose();
  });

  test("an inactive pane resumes with the advance the loaded font paints", async () => {
    const fixture = mountTerminalFontsFixture();
    settlePublications();
    expect(claimSequence(fixture.takeClaims())).toEqual([{ cols: 40, rows: 10 }]);

    // Backgrounded pane: the page hides and the view leaves layout, then the
    // webfont swaps in behind it with a wider advance than the fallback face.
    terminalPageVisible = false;
    fixture.setViewActive(false);
    fixture.setProbeCell(10, 20);
    fixture.settleFontsReady();
    await flushFontCompletion();
    settlePublications();
    expect(fixture.takeClaims()).toEqual([]);

    terminalPageVisible = true;
    fixture.setViewActive(true);
    settlePublications();

    expect(claimSequence(fixture.takeClaims())).toEqual([{ cols: 32, rows: 8 }]);
    fixture.dispose();
  });

  test("a pending pane's admitted claim uses the advance the loaded font paints", async () => {
    const fixture = mountTerminalFontsFixture();
    settlePublications();
    expect(claimSequence(fixture.takeClaims())).toEqual([{ cols: 40, rows: 10 }]);

    fixture.setPending(true);
    fixture.setProbeCell(10, 20);
    fixture.settleFontsReady();
    await flushFontCompletion();
    settlePublications();
    expect(fixture.takeClaims()).toEqual([]);

    fixture.setPending(false);
    settlePublications();

    expect(claimSequence(fixture.takeClaims())).toEqual([{ cols: 32, rows: 8 }]);
    fixture.dispose();
  });

  test("a later loading epoch reclaims an active pane without a resize", async () => {
    const fixture = mountTerminalFontsFixture();
    fixture.settleFontsReady();
    await flushFontCompletion();
    settlePublications();
    expect(claimSequence(fixture.takeClaims())).toEqual([{ cols: 40, rows: 10 }]);

    fixture.setProbeCell(16, 20);
    fixture.dispatchFontLoadingEvent("loadingdone");
    await flushFontCompletion();
    settlePublications();

    expect(claimSequence(fixture.takeClaims())).toEqual([{ cols: 20, rows: 8 }]);
    fixture.dispose();
  });

  test("a failed font load reclaims the advance the fallback face paints", async () => {
    const fixture = mountTerminalFontsFixture();
    fixture.settleFontsReady();
    await flushFontCompletion();
    settlePublications();
    expect(claimSequence(fixture.takeClaims())).toEqual([{ cols: 40, rows: 10 }]);

    fixture.setProbeCell(20, 20);
    fixture.dispatchFontLoadingEvent("loadingerror");
    await flushFontCompletion();
    settlePublications();

    expect(claimSequence(fixture.takeClaims())).toEqual([{ cols: 16, rows: 8 }]);
    fixture.dispose();
  });

  test("font completion after disposal claims nothing and retries nothing", async () => {
    const fixture = mountTerminalFontsFixture();
    settlePublications();
    expect(claimSequence(fixture.takeClaims())).toEqual([{ cols: 40, rows: 10 }]);

    fixture.dispose();
    fixture.setProbeCell(10, 20);
    fixture.settleFontsReady();
    await flushFontCompletion();
    fixture.dispatchFontLoadingEvent("loadingdone");
    await flushFontCompletion();
    settlePublications();

    expect(fixture.takeClaims()).toEqual([]);
  });
});
