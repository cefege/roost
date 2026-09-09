// Verifies shared document lifecycle ownership for mounted CellTerminal controllers.
// The fake document keeps this focused on listener routing, reader parking, and lease refresh.
// Real Solid effects exercise the same lifecycle mount path used by terminal panes.

import { afterEach, describe, expect, mock, test } from "bun:test";
import type * as SolidApi from "solid-js";

class TrackingEventTarget extends EventTarget {
  private readonly listenersByType = new Map<
    string,
    Set<EventListenerOrEventListenerObject>
  >();

  override addEventListener(
    type: string,
    callback: EventListenerOrEventListenerObject | null,
    options?: AddEventListenerOptions | boolean,
  ): void {
    if (callback) {
      const listeners = this.listenersByType.get(type)
        ?? new Set<EventListenerOrEventListenerObject>();
      listeners.add(callback);
      this.listenersByType.set(type, listeners);
    }
    super.addEventListener(type, callback, options);
  }

  override removeEventListener(
    type: string,
    callback: EventListenerOrEventListenerObject | null,
    options?: EventListenerOptions | boolean,
  ): void {
    if (callback) {
      const listeners = this.listenersByType.get(type);
      listeners?.delete(callback);
      if (listeners?.size === 0) this.listenersByType.delete(type);
    }
    super.removeEventListener(type, callback, options);
  }

  listenerCount(type: string): number {
    return this.listenersByType.get(type)?.size ?? 0;
  }
}

class FakeResizeObserver {
  disconnect(): void {}
  observe(): void {}
}

const fakeDocument = Object.assign(new TrackingEventTarget(), {
  activeElement: null as Element | null,
  body: {} as HTMLElement,
  documentElement: {} as HTMLElement,
  hasFocus: () => false,
  visibilityState: "visible" as DocumentVisibilityState,
});
const fakeWindow = new TrackingEventTarget();
Object.assign(globalThis, {
  ResizeObserver: FakeResizeObserver,
  cancelAnimationFrame: () => undefined,
  document: fakeDocument,
  requestAnimationFrame: () => 1,
  window: fakeWindow,
});

let visible = true;
// Bun resolves the public Solid entry to SSR, so load the browser runtime only
// after fake DOM globals exist; lifecycle modules load after their adapter mocks.
const solidClientUrl = new URL("./solid.js", import.meta.resolve("solid-js"));
const Solid = await import(solidClientUrl.href) as typeof SolidApi;
mock.module("solid-js", () => ({ ...Solid }));
mock.module("@roost/shared/diag", () => ({ diag: () => undefined }));
mock.module("../src/lib/focusOwners.ts", () => ({
  FOCUS_OWNERS: "[data-focus-owner]",
}));
mock.module("../src/lib/pageVisible.ts", () => ({
  isPageVisible: () => visible,
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

interface MountedLifecycle {
  readonly events: string[];
  dispose(): void;
}

let mountedLifecycles: MountedLifecycle[] = [];

function mountLifecycle(
  sessionId: string,
  publishViewportNow = true,
): MountedLifecycle {
  const [viewActive] = Solid.createSignal(true);
  const events: string[] = [];
  const runtime = {
    cellHeight: 16,
    cellWidth: 8,
    display: () => ({} as HTMLDivElement),
    inputController: null,
    renderer: null,
    revealStartedAt: 0,
    sessionId,
    view: { refresh: () => events.push("refresh") },
  };
  const presentation = {
    clearFrameActivity: () => events.push("clear-frame-activity"),
    notifyBackfill: () => undefined,
    refreshTerminalPresentation: () => events.push("refresh-presentation"),
    releasePaintHolds: () => events.push("release-paint-holds"),
  };
  const viewport = {
    cancelScheduled: () => undefined,
    parkView: () => events.push("park-view"),
    publishInactive: () => events.push("publish-inactive"),
    publishViewport: () => false,
    publishViewportNow: () => {
      events.push("remeasure-viewport");
      return publishViewportNow;
    },
    scheduleViewport: () => undefined,
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
        session: { git_remote: null, id: sessionId },
        spotlit: false,
      } as never,
      runtime as never,
      input as never,
      presentation as never,
      viewport as never,
      () => false,
    );
  });
  let disposed = false;
  const mounted: MountedLifecycle = {
    events,
    dispose: () => {
      if (disposed) return;
      disposed = true;
      lifecycle?.dispose();
      disposeRoot();
    },
  };
  mountedLifecycles.push(mounted);
  events.length = 0;
  return mounted;
}

afterEach(() => {
  for (const lifecycle of mountedLifecycles) lifecycle.dispose();
  mountedLifecycles = [];
  visible = true;
});

describe("cell terminal document lifecycle", () => {
  test("owns one document listener set for every mounted terminal lifecycle", () => {
    const first = mountLifecycle("session-first");
    const second = mountLifecycle("session-second");

    expect(fakeDocument.listenerCount("visibilitychange")).toBe(1);
    expect(fakeDocument.listenerCount("resume")).toBe(1);
    expect(fakeWindow.listenerCount("pagehide")).toBe(1);
    expect(fakeWindow.listenerCount("pageshow")).toBe(1);

    first.dispose();
    expect(fakeDocument.listenerCount("visibilitychange")).toBe(1);
    expect(fakeWindow.listenerCount("pageshow")).toBe(1);

    second.dispose();
    expect(fakeDocument.listenerCount("visibilitychange")).toBe(0);
    expect(fakeDocument.listenerCount("resume")).toBe(0);
    expect(fakeWindow.listenerCount("pagehide")).toBe(0);
    expect(fakeWindow.listenerCount("pageshow")).toBe(0);
  });

  test("parks reader and view intent on hidden and pagehide", () => {
    const first = mountLifecycle("session-first");
    const second = mountLifecycle("session-second");

    visible = false;
    fakeDocument.dispatchEvent(new Event("visibilitychange"));
    expect(first.events).toEqual(["park-view"]);
    expect(second.events).toEqual(["park-view"]);

    first.events.length = 0;
    second.events.length = 0;
    fakeWindow.dispatchEvent(new Event("pagehide"));
    expect(first.events).toEqual([
      "clear-frame-activity",
      "release-paint-holds",
      "publish-inactive",
    ]);
    expect(second.events).toEqual([
      "clear-frame-activity",
      "release-paint-holds",
      "publish-inactive",
    ]);
  });

  test("refreshes active leases after pageshow and resume when remeasurement retains geometry", () => {
    const lifecycle = mountLifecycle("session-refresh", false);

    fakeWindow.dispatchEvent(new Event("pageshow"));
    fakeDocument.dispatchEvent(new Event("resume"));

    expect(lifecycle.events).toEqual([
      "refresh-presentation",
      "remeasure-viewport",
      "refresh",
      "refresh-presentation",
      "remeasure-viewport",
      "refresh",
    ]);
  });

  test("keeps a visible active pane live when its document is unfocused", () => {
    const lifecycle = mountLifecycle("session-unfocused");
    expect(fakeDocument.hasFocus()).toBe(false);

    fakeDocument.dispatchEvent(new Event("visibilitychange"));

    expect(lifecycle.events).toEqual([
      "refresh-presentation",
      "remeasure-viewport",
      "refresh",
    ]);
  });

  test("stops routing document lifecycle work after disposal", () => {
    const lifecycle = mountLifecycle("session-disposed");
    lifecycle.dispose();
    lifecycle.events.length = 0;

    fakeDocument.dispatchEvent(new Event("visibilitychange"));
    fakeDocument.dispatchEvent(new Event("resume"));
    fakeWindow.dispatchEvent(new Event("pagehide"));
    fakeWindow.dispatchEvent(new Event("pageshow"));

    expect(lifecycle.events).toEqual([]);
    expect(fakeDocument.listenerCount("visibilitychange")).toBe(0);
    expect(fakeWindow.listenerCount("pageshow")).toBe(0);
  });
});
