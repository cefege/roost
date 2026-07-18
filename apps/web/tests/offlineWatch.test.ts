// offlineWatch — the state machine behind CellTerminal's "terminal isn't
// responding" overlay. A pane that is VIEWED (in layout + tab foregrounded) but
// never receives a screen frame within the grace window is a dead breadcrumb;
// anything else is healthy. The overlay must never flash on a live pane and
// must clear the instant a frame lands. Fake timers drive the grace window.

import { expect, test, describe, beforeEach, afterEach, vi } from "bun:test";
import { createOfflineWatch } from "../src/lib/offlineWatch.ts";

const GRACE = 3000;

function harness() {
  const changes: boolean[] = [];
  const w = createOfflineWatch(GRACE, (v) => changes.push(v));
  return { changes, ...w };
}

function retryHarness(retries?: number) {
	const changes: boolean[] = [];
	let retriesFired = 0;
	const w = createOfflineWatch(
		GRACE,
		(v) => changes.push(v),
		() => {
			retriesFired++;
		},
		retries,
	);
	return { changes, retried: () => retriesFired, ...w };
}

describe("createOfflineWatch", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  test("viewed + no frame past grace → offline (fires once)", () => {
    const h = harness();
    h.update(true, false);
    vi.advanceTimersByTime(GRACE - 1);
    expect(h.changes).toEqual([]); // still within grace
    vi.advanceTimersByTime(2);
    expect(h.changes).toEqual([true]);
  });

  test("frame arrives before grace → never offline", () => {
    const h = harness();
    h.update(true, false); // viewed, waiting for first frame
    vi.advanceTimersByTime(1000);
    h.update(true, true); // frame landed
    vi.advanceTimersByTime(5 * GRACE);
    expect(h.changes).toEqual([]);
  });

  test("never viewed → never offline (backgrounded pane doesn't accuse)", () => {
    const h = harness();
    h.update(false, false);
    vi.advanceTimersByTime(5 * GRACE);
    expect(h.changes).toEqual([]);
  });

  test("offline then a frame arrives → clears", () => {
    const h = harness();
    h.update(true, false);
    vi.advanceTimersByTime(GRACE);
    expect(h.changes).toEqual([true]);
    h.update(true, true); // retry succeeded / late replay
    expect(h.changes).toEqual([true, false]);
  });

  test("offline then pane un-viewed → clears (overlay hides on nav away)", () => {
    const h = harness();
    h.update(true, false);
    vi.advanceTimersByTime(GRACE);
    h.update(false, false);
    expect(h.changes).toEqual([true, false]);
  });

  test("proven-live pane (hasFrame from the start) never arms", () => {
    const h = harness();
    h.update(true, true);
    vi.advanceTimersByTime(5 * GRACE);
    expect(h.changes).toEqual([]);
  });

  test("repeated identical updates don't restart the grace or double-fire", () => {
    const h = harness();
    h.update(true, false); // arm at t=0
    vi.advanceTimersByTime(1500);
    h.update(true, false); // must NOT restart the timer
    vi.advanceTimersByTime(1500); // total 3000 since first arm
    expect(h.changes).toEqual([true]); // fired once, on the original schedule
  });

  test("dispose cancels a pending accusation", () => {
    const h = harness();
    h.update(true, false);
    h.dispose();
    vi.advanceTimersByTime(5 * GRACE);
    expect(h.changes).toEqual([]);
  });

  test("retries silently on each grace, then declares offline", () => {
    const h = retryHarness();
    h.update(true, false);
    vi.advanceTimersByTime(GRACE);
    expect(h.retried()).toBe(1);
    expect(h.changes).toEqual([]); // silent retry, no accusation
    vi.advanceTimersByTime(GRACE);
    expect(h.retried()).toBe(2);
    expect(h.changes).toEqual([]);
    vi.advanceTimersByTime(GRACE);
    expect(h.changes).toEqual([true]); // budget spent → offline
    expect(h.retried()).toBe(2); // no retry fired on the final expiry
  });

  test("frame after a silent retry heals without ever going offline", () => {
    const h = retryHarness();
    h.update(true, false);
    vi.advanceTimersByTime(GRACE);
    expect(h.retried()).toBe(1);
    h.update(true, true); // the retry's snapshot landed
    vi.advanceTimersByTime(5 * GRACE);
    expect(h.changes).toEqual([]);
  });

  test("retry budget resets when the pane is un-viewed", () => {
    const h = retryHarness();
    h.update(true, false);
    vi.advanceTimersByTime(GRACE);
    expect(h.retried()).toBe(1);
    h.update(false, false); // navigated away → clears + resets budget
    h.update(true, false); // viewed again → fresh budget
    vi.advanceTimersByTime(3 * GRACE);
    expect(h.retried()).toBe(3); // 1 earlier + 2 fresh
    expect(h.changes).toEqual([true]);
  });

  test("dispose mid-retry cancels further retries and the accusation", () => {
    const h = retryHarness();
    h.update(true, false);
    vi.advanceTimersByTime(GRACE);
    expect(h.retried()).toBe(1);
    h.dispose();
    vi.advanceTimersByTime(5 * GRACE);
    expect(h.retried()).toBe(1);
    expect(h.changes).toEqual([]);
  });

  test("retries=0 with onRetry → offline at first expiry, zero retries", () => {
    const h = retryHarness(0);
    h.update(true, false);
    vi.advanceTimersByTime(GRACE);
    expect(h.changes).toEqual([true]);
    expect(h.retried()).toBe(0);
  });
});
