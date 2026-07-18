// fabDragOffset drag-math tripwire. No jsdom (repo convention, see
// cellRenderer.dom.test.ts) — a ~30-line fake DOM covers exactly what the
// module touches: window event registry + innerHeight, documentElement.style
// setProperty, localStorage. Asserts the three load-bearing behaviors:
//   1. drag UP raises --roost-fab-dy by (startY - clientY), clamped on-screen,
//   2. a below-threshold move is a TAP — no offset, no persist, no click-swallow,
//   3. an armed release persists + installs the one-shot click-swallow listener.
// Globals must exist BEFORE importing the module (it reads them at eval).

import { describe, test, expect, beforeEach } from "bun:test";

type Handler = (ev: any) => void;

function installFakeDom(innerHeight: number) {
  const listeners = new Map<string, Set<Handler>>();
  const cssProps: Record<string, string> = {};
  const store = new Map<string, string>();

  const win = {
    innerHeight,
    addEventListener(type: string, fn: Handler) {
      (listeners.get(type) ?? listeners.set(type, new Set()).get(type)!).add(fn);
    },
    removeEventListener(type: string, fn: Handler) {
      listeners.get(type)?.delete(fn);
    },
    fire(type: string, ev: any) {
      for (const fn of [...(listeners.get(type) ?? [])]) fn(ev);
    },
    count(type: string) {
      return listeners.get(type)?.size ?? 0;
    },
    // rAF stub — the drag handler now coalesces var-writes to one per frame;
    // flushRaf drains the queued callbacks so tests can assert synchronously.
    _rafQueue: [] as Array<(() => void) | null>,
    _rafNext: 1,
    requestAnimationFrame(cb: () => void): number {
      const id = this._rafNext++;
      this._rafQueue[id] = cb;
      return id;
    },
    cancelAnimationFrame(id: number): void {
      this._rafQueue[id] = null;
    },
    flushRaf(): void {
      for (const cb of this._rafQueue) if (cb) cb();
      this._rafQueue = [];
      this._rafNext = 1;
    },
  };

  (globalThis as any).window = win;
  // The module calls bare requestAnimationFrame/cancelAnimationFrame (globals),
  // so expose the win stub's methods on globalThis — same queue, flushed via flushRaf.
  (globalThis as any).requestAnimationFrame = (cb: () => void) => win.requestAnimationFrame(cb);
  (globalThis as any).cancelAnimationFrame = (id: number) => win.cancelAnimationFrame(id);
  (globalThis as any).document = {
    documentElement: { style: { setProperty: (n: string, v: string) => (cssProps[n] = v) } },
  };
  (globalThis as any).localStorage = {
    getItem: (k: string) => store.get(k) ?? null,
    setItem: (k: string, v: string) => store.set(k, v),
  };

  return { win, cssProps, store };
}

// Fresh module instance per test: cache-bust the import specifier so module-top
// `load()`/`setVar()` re-runs against the freshly installed fake DOM.
async function freshModule() {
  return (await import(`../src/lib/fabDragOffset.ts?t=${Math.random()}`)) as {
    onFabPointerDown: (e: any) => void;
  };
}

const VAR = "--roost-fab-dy";
const pd = (clientY: number, clientX = 500) => ({ pointerType: "mouse", button: 0, clientX, clientY });

describe("fabDragOffset", () => {
  beforeEach(() => installFakeDom(800)); // max offset = 800 - 120 = 680

  test("drag up raises the offset var by (startY - clientY)", async () => {
    const dom = installFakeDom(800);
    const { onFabPointerDown } = await freshModule();
    expect(dom.cssProps[VAR]).toBe("0px"); // applied at import

    onFabPointerDown(pd(700));
    dom.win.fire("pointermove", pd(500)); // up 200px
    dom.win.flushRaf(); // coalesced var-write deferred to a frame
    expect(dom.cssProps[VAR]).toBe("200px");

    dom.win.fire("pointerup", {});
    expect(dom.store.get("roost.fabOffsetY.v1")).toBe("200"); // persisted
    expect(dom.win.count("click")).toBe(1); // one-shot swallow installed
  });

  test("clamps so the cluster can't leave the top of the viewport", async () => {
    const dom = installFakeDom(800);
    const { onFabPointerDown } = await freshModule();
    onFabPointerDown(pd(700));
    dom.win.fire("pointermove", pd(0)); // would be +700, clamps to 680
    dom.win.flushRaf(); // coalesced var-write deferred to a frame
    expect(dom.cssProps[VAR]).toBe("680px");
  });

  test("below-threshold move is a tap: no offset change, no persist, no swallow", async () => {
    const dom = installFakeDom(800);
    const { onFabPointerDown } = await freshModule();
    onFabPointerDown(pd(700));
    dom.win.fire("pointermove", pd(697)); // 3px < 5px threshold
    dom.win.fire("pointerup", {});
    expect(dom.cssProps[VAR]).toBe("0px");
    expect(dom.store.get("roost.fabOffsetY.v1")).toBeUndefined();
    expect(dom.win.count("click")).toBe(0); // tap → FAB's own onClick fires
  });

  test("restores a persisted offset at import (before first paint)", async () => {
    const dom = installFakeDom(800);
    dom.store.set("roost.fabOffsetY.v1", "150");
    await freshModule();
    expect(dom.cssProps[VAR]).toBe("150px");
  });
});
