// Withdraw symmetry for one mounted cell terminal: a claim is debounced, so a
// withdraw caused by the deck's zero-sized ResizeObserver tick is graced rather
// than published instantly. Publishing it re-mints the session's smallest common
// geometry, reframing every OTHER viewer for a pane that never left. Every real
// hide — overlay route, spotlight scrim, hidden page, pagehide, dispose — still
// withdraws on the spot, including while a grace is armed.

import { afterEach, beforeEach, describe, expect, mock, test, vi } from "bun:test";
import type * as SolidApi from "solid-js";
import {
  createCellTerminalViewport,
  type CellTerminalViewport,
} from "../src/components/cell-terminal-viewport.ts";

interface FakeBox {
  clientWidth: number;
  clientHeight: number;
}

class FakeResizeObserver {
  static observers: FakeResizeObserver[] = [];
  constructor(readonly callback: () => void) {
    FakeResizeObserver.observers.push(this);
  }
  disconnect(): void {}
  observe(): void {}
}

// `document.fonts` is a FontFaceSet: an EventTarget whose `ready` answers one
// loading epoch. These tests never settle an epoch, so a pane keeps the cell
// metrics this fixture seeds and only withdraw symmetry moves; metric
// invalidation is pinned in cellTerminalLifecycle.fonts.test.ts.
class FakeFontFaceSet extends EventTarget {
  ready = new Promise<void>(() => undefined);
}

class FakeLifecycleDocument extends EventTarget {
  activeElement: Element | null = null;
  body = {} as HTMLElement;
  deck: FakeBox | null = { clientWidth: 1_200, clientHeight: 800 };
  documentElement = {} as HTMLElement;
  fonts = new FakeFontFaceSet();
  visibilityState = "visible" as DocumentVisibilityState;

  hasFocus(): boolean {
    return true;
  }

  // The source reads the deck by its stable testid anchor; anything else must
  // miss, so a renamed anchor fails here instead of silently never gracing.
  querySelector(selector: string): unknown {
    return selector === '[data-testid="terminal-deck"]' ? this.deck : null;
  }
}

const browserGlobals = globalThis as unknown as Record<string, unknown>;
let savedGlobals: Record<string, unknown>;
let lifecycleDocument: FakeLifecycleDocument;
let lifecycleWindow: EventTarget;
let lifecycleVisible = true;

browserGlobals.ResizeObserver = FakeResizeObserver;
browserGlobals.document = new FakeLifecycleDocument();
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

const { mountCellTerminalLifecycle } = await import(
  "../src/components/cell-terminal-lifecycle.ts"
);

interface PaneFixture {
  /** Every view intent this pane published, in order. */
  readonly intents: string[];
  readonly viewport: CellTerminalViewport;
  readonly display: FakeBox;
  collapseDeck(): void;
  restoreDeck(): void;
  setInLayout(inLayout: boolean): void;
  setSurfaceVisible(visible: boolean): void;
  setSurfaceActive(active: boolean): void;
  notifyDisplayResize(): void;
  dispose(): void;
}

function mountPaneFixture(): PaneFixture {
  const [inLayout, setInLayout] = Solid.createSignal(true);
  const [surfaceVisible, setSurfaceVisible] = Solid.createSignal(true);
  const [surfaceActive, setSurfaceActive] = Solid.createSignal(true);
  const intents: string[] = [];
  const display: FakeBox = { clientWidth: 800, clientHeight: 400 };
  const runtime = {
    backfill: null,
    cellHeight: 20,
    cellWidth: 10,
    display: () => display as unknown as HTMLDivElement,
    inputController: null,
    renderer: null,
    revealStartedAt: 0,
    sessionId: "grace-session",
    unmounted: false,
    view: {
      viewId: "grace-view",
      refresh: () => undefined,
      setInactive: () => intents.push("inactive"),
      setViewport: (geometry: { cols: number; rows: number }) =>
        intents.push(`active ${geometry.cols}x${geometry.rows}`),
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
  const props = {
    focused: true,
    session: { git_remote: null, id: "grace-session" },
    spotlit: false,
    get inLayout() {
      return inLayout();
    },
    get surfaceVisible() {
      return surfaceVisible();
    },
    get surfaceActive() {
      return surfaceActive();
    },
  };
  let viewport!: CellTerminalViewport;
  let lifecycle: { dispose(): void } | null = null;
  let disposeRoot = (): void => undefined;
  Solid.createRoot((dispose) => {
    disposeRoot = dispose;
    const viewActive = Solid.createMemo(() =>
      props.inLayout === true && props.surfaceVisible && props.surfaceActive);
    viewport = createCellTerminalViewport(
      runtime as never,
      presentation as never,
      () => false,
      viewActive,
    );
    lifecycle = mountCellTerminalLifecycle(
      props as never,
      runtime as never,
      input as never,
      presentation as never,
      viewport,
      () => false,
    );
  });
  intents.length = 0;
  let disposed = false;
  return {
    intents,
    viewport,
    display,
    collapseDeck: () => {
      lifecycleDocument.deck = { clientWidth: 0, clientHeight: 0 };
    },
    restoreDeck: () => {
      lifecycleDocument.deck = { clientWidth: 1_200, clientHeight: 800 };
    },
    setInLayout,
    setSurfaceVisible,
    setSurfaceActive,
    notifyDisplayResize: () => {
      for (const observer of FakeResizeObserver.observers) observer.callback();
    },
    dispose: () => {
      if (disposed) return;
      disposed = true;
      lifecycle?.dispose();
      disposeRoot();
    },
  };
}

function withdrawals(fixture: PaneFixture): number {
  return fixture.intents.filter((intent) => intent === "inactive").length;
}

beforeEach(() => {
  vi.useFakeTimers();
  savedGlobals = {
    document: browserGlobals.document,
    getComputedStyle: browserGlobals.getComputedStyle,
    requestAnimationFrame: browserGlobals.requestAnimationFrame,
    cancelAnimationFrame: browserGlobals.cancelAnimationFrame,
    window: browserGlobals.window,
  };
  FakeResizeObserver.observers = [];
  lifecycleDocument = new FakeLifecycleDocument();
  lifecycleWindow = new EventTarget();
  browserGlobals.document = lifecycleDocument;
  browserGlobals.window = lifecycleWindow;
  browserGlobals.getComputedStyle = () => ({
    paddingLeft: "0px",
    paddingRight: "0px",
    paddingTop: "0px",
    paddingBottom: "0px",
  });
  browserGlobals.requestAnimationFrame = (): number => 1;
  browserGlobals.cancelAnimationFrame = (): void => undefined;
});

afterEach(() => {
  vi.useRealTimers();
  lifecycleVisible = true;
  for (const [key, value] of Object.entries(savedGlobals)) browserGlobals[key] = value;
});

describe("transient layout gap", () => {
  test("a zero-sized deck never withdraws a pane that returns inside the grace", () => {
    const fixture = mountPaneFixture();

    fixture.collapseDeck();
    fixture.setInLayout(false);
    expect(withdrawals(fixture)).toBe(0);

    vi.advanceTimersByTime(120);
    expect(withdrawals(fixture)).toBe(0);

    fixture.restoreDeck();
    fixture.setInLayout(true);
    vi.advanceTimersByTime(1_000);

    expect(withdrawals(fixture)).toBe(0);
    expect(fixture.intents.at(-1)).toBe("active 80x20");
    fixture.dispose();
  });

  test("a display resize inside the grace cannot park the graced withdraw", () => {
    const fixture = mountPaneFixture();

    fixture.collapseDeck();
    fixture.setInLayout(false);
    fixture.display.clientWidth = 0;
    fixture.display.clientHeight = 0;
    fixture.notifyDisplayResize();
    vi.advanceTimersByTime(60);

    expect(withdrawals(fixture)).toBe(0);
    fixture.dispose();
  });

  test("a deck that stays collapsed withdraws once, after the grace", () => {
    const fixture = mountPaneFixture();

    fixture.collapseDeck();
    fixture.setInLayout(false);
    vi.advanceTimersByTime(249);
    expect(withdrawals(fixture)).toBe(0);

    vi.advanceTimersByTime(1);
    expect(withdrawals(fixture)).toBe(1);

    vi.advanceTimersByTime(5_000);
    expect(withdrawals(fixture)).toBe(1);
    fixture.dispose();
  });

  test("leaving layout while the deck is measured withdraws immediately", () => {
    const fixture = mountPaneFixture();

    fixture.setInLayout(false);

    expect(withdrawals(fixture)).toBe(1);
    fixture.dispose();
  });
});

describe("real hide", () => {
  test("an overlay route withdraws immediately even while the deck measures zero", () => {
    const fixture = mountPaneFixture();

    fixture.collapseDeck();
    fixture.setSurfaceVisible(false);

    expect(withdrawals(fixture)).toBe(1);
    fixture.dispose();
  });

  test("another pane's spotlight withdraws immediately even while the deck measures zero", () => {
    const fixture = mountPaneFixture();

    fixture.collapseDeck();
    fixture.setSurfaceActive(false);

    expect(withdrawals(fixture)).toBe(1);
    fixture.dispose();
  });

  test("a hidden document supersedes an armed grace", () => {
    const fixture = mountPaneFixture();

    fixture.collapseDeck();
    fixture.setInLayout(false);
    expect(withdrawals(fixture)).toBe(0);

    lifecycleVisible = false;
    lifecycleDocument.visibilityState = "hidden";
    lifecycleDocument.dispatchEvent(new Event("visibilitychange"));

    expect(withdrawals(fixture)).toBe(1);
    vi.advanceTimersByTime(1_000);
    expect(withdrawals(fixture)).toBe(1);
    fixture.dispose();
  });

  test("pagehide supersedes an armed grace", () => {
    const fixture = mountPaneFixture();

    fixture.collapseDeck();
    fixture.setInLayout(false);
    lifecycleWindow.dispatchEvent(new Event("pagehide"));

    expect(withdrawals(fixture)).toBe(1);
    vi.advanceTimersByTime(1_000);
    expect(withdrawals(fixture)).toBe(1);
    fixture.dispose();
  });

  test("dispose supersedes an armed grace", () => {
    const fixture = mountPaneFixture();

    fixture.collapseDeck();
    fixture.setInLayout(false);
    fixture.dispose();

    expect(withdrawals(fixture)).toBe(1);
    vi.advanceTimersByTime(1_000);
    expect(withdrawals(fixture)).toBe(1);
  });
});
