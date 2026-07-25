// sessionSeen — per-session "last seen" stamps behind the Needs-you band.
// Locks the perf-sweep contract: markSeen is monotonic and the no-op branch
// is completely silent (no store write, no persist scheduled); real writes
// persist via ONE trailing 500 ms debounced localStorage write per burst;
// a pending persist survives tab close via the pagehide flush.
//
// Deviation from the plan's "does not notify a createEffect subscriber of
// another id" phrasing: Solid resolves to its SSR build under `bun test`
// (createEffect is inert — see deadRouteSafetyNet.test.ts), so reactive
// notification isn't observable here. The testable half of that contract is
// what this file asserts: the no-op branch performs NO state change and NO
// persist. Per-key subscription is delivered by createStore (browser build).

import { expect, test, describe, beforeEach, afterEach, vi } from "bun:test";

// bun test has no localStorage/window — stub BEFORE importing the module
// under test (it reads storage and registers pagehide at import time).
const _ls: Record<string, string> = {};
const _setItemLog: string[] = [];
const _pagehideHandlers: (() => void)[] = [];
// Named-cast globals: test doubles for browser APIs bun doesn't provide.
const g = globalThis as unknown as {
  localStorage: Storage;
  window: { addEventListener: (ev: string, fn: () => void) => void };
};
g.localStorage = {
  getItem: (k: string) => _ls[k] ?? null,
  setItem: (k: string, v: string) => { _setItemLog.push(k); _ls[k] = v; },
  removeItem: (k: string) => { delete _ls[k]; },
  clear: () => { for (const k of Object.keys(_ls)) delete _ls[k]; },
  key: () => null, length: 0,
} as Storage;
g.window = {
  addEventListener: (ev: string, fn: () => void) => {
    if (ev === "pagehide") _pagehideHandlers.push(fn);
  },
};

// Dynamic import on purpose: the module must initialize AFTER the stubs above.
// __resetPersistForTests rewinds the shared-registry state an earlier file may
// have left behind (see beforeEach) — import order must not decide the result.
const { lastSeenAt, markSeen, seedSeenOnce, __resetPersistForTests } =
  await import("../src/lib/sessionSeen.ts");

const SEEN_KEY = "roost.sidebar.seen";

function seenWrites(): number {
  return _setItemLog.filter((k) => k === SEEN_KEY).length;
}

describe("sessionSeen", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    // Another test file may have imported this module and armed its debounce
    // before our window/localStorage stubs existed; rewind so the timer and
    // the pagehide hook belong to THIS suite.
    __resetPersistForTests();
    _pagehideHandlers.length = 0;
    _setItemLog.length = 0;
  });
  // Drain any pending debounce inside the SAME fake-timer session so no
  // module-level timer leaks into the next test, then restore real timers.
  afterEach(() => {
    vi.advanceTimersByTime(1000);
    vi.useRealTimers();
  });

  test("markSeen is monotonic; the no-op branch writes nothing at all", () => {
    markSeen("s1", 1000);
    expect(lastSeenAt("s1")).toBe(1000);
    vi.advanceTimersByTime(500); // drain the real write
    const writesAfterRealUpdate = seenWrites();
    expect(writesAfterRealUpdate).toBe(1);

    markSeen("s1", 500);  // older → no-op
    markSeen("s1", 1000); // equal → no-op
    expect(lastSeenAt("s1")).toBe(1000); // stamp never moves backward
    vi.advanceTimersByTime(1000);
    expect(seenWrites()).toBe(writesAfterRealUpdate); // no persist scheduled
  });

  test("a burst of markSeen persists localStorage ONCE, trailing 500ms", () => {
    markSeen("a", 100);
    markSeen("b", 200);
    markSeen("c", 300);
    expect(seenWrites()).toBe(0); // nothing synchronous
    vi.advanceTimersByTime(499);
    expect(seenWrites()).toBe(0); // still inside the debounce window
    vi.advanceTimersByTime(2);
    expect(seenWrites()).toBe(1); // ONE stringify for the whole burst
    const stored = JSON.parse(_ls[SEEN_KEY]) as Record<string, number>;
    expect(stored).toMatchObject({ a: 100, b: 200, c: 300 });
  });

  test("pagehide flushes a pending persist immediately (and only once)", () => {
    markSeen("p1", 42_000);
    // Registered lazily on first schedule — import order across the shared
    // bun-test process must not matter (another test file may import
    // sessionSeen transitively before this file's window stub installs).
    expect(_pagehideHandlers.length).toBeGreaterThan(0);
    expect(seenWrites()).toBe(0); // debounce still pending
    for (const fn of _pagehideHandlers) fn();
    expect(seenWrites()).toBe(1); // flushed synchronously on tab close
    expect((JSON.parse(_ls[SEEN_KEY]) as Record<string, number>)["p1"]).toBe(42_000);
    vi.advanceTimersByTime(1000);
    expect(seenWrites()).toBe(1); // flush cleared the timer — no double write
  });

  test("seedSeenOnce stamps once ever (localStorage marker)", () => {
    expect(lastSeenAt("seed1")).toBe(0);
    seedSeenOnce(["seed1"]);
    expect(lastSeenAt("seed1")).toBeGreaterThan(0);
    seedSeenOnce(["seed2"]); // marker set → whole call is a no-op
    expect(lastSeenAt("seed2")).toBe(0);
  });
});
