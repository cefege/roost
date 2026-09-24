// offlineWatch — the state machine behind CellTerminal's "terminal isn't
// responding" overlay and the silent re-claims that precede it. A pane may be
// accused only while the operator looks at a view that has stopped being
// deliverable (`detached`) with nothing painting; a quiet shell behind a healthy
// view prints nothing for hours and must never be accused, and a frame that
// paints beats whatever the view status claims. Fake timers drive the grace.

import { expect, test, describe, beforeEach, afterEach, vi } from "bun:test";
import { createOfflineWatch } from "../../src/browser/offlineWatch.ts";
import {
  DETACHED_GRACE_MS,
  FRAME_ACTIVITY_WINDOW_MS,
} from "../../src/store/terminal-stream-types.ts";

const GRACE = 3000;

/** The three facts CellTerminal feeds: the operator is looking, the view reads
 *  `detached`, and a frame painted inside the freshness window. */
interface PaneFacts {
  viewed: boolean;
  detached: boolean;
  painted: boolean;
}

/** Viewed, the view can no longer deliver, nothing painting: the ONLY shape
 *  that may ever be accused. */
const unreachableView: PaneFacts = { viewed: true, detached: true, painted: false };
/** Viewed, healthy view, frames landing. */
const paintingView: PaneFacts = { viewed: true, detached: false, painted: true };
/** Viewed, healthy view, a shell with nothing to print. */
const quietHealthyView: PaneFacts = { viewed: true, detached: false, painted: false };
/** Nobody is looking while the view is broken (navigated away, tab hidden). */
const unwatchedBrokenView: PaneFacts = { viewed: false, detached: true, painted: false };
/** A frame paints while the view status still claims the view is gone. */
const paintingDetachedView: PaneFacts = { viewed: true, detached: true, painted: true };

function watchHarness(onRetry?: () => void, retries?: number) {
  const changes: boolean[] = [];
  const watch = createOfflineWatch(GRACE, (v) => changes.push(v), onRetry, retries);
  return {
    changes,
    feed: (facts: PaneFacts): void =>
      watch.update(facts.viewed, facts.detached, facts.painted),
    dispose: watch.dispose,
  };
}

function retryHarness(retries?: number) {
  let reclaims = 0;
  const watch = watchHarness(() => { reclaims += 1; }, retries);
  return { ...watch, reclaimed: () => reclaims };
}

describe("createOfflineWatch", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  test("viewed + undeliverable view past grace → offline (fires once)", () => {
    const h = watchHarness();
    h.feed(unreachableView);
    vi.advanceTimersByTime(GRACE - 1);
    expect(h.changes).toEqual([]); // still within grace
    vi.advanceTimersByTime(2);
    expect(h.changes).toEqual([true]);
  });

  test("a frame paints before the grace → never offline", () => {
    const h = watchHarness();
    h.feed(unreachableView); // viewed, waiting for the view to deliver
    vi.advanceTimersByTime(1000);
    h.feed(paintingView); // frame landed
    vi.advanceTimersByTime(5 * GRACE);
    expect(h.changes).toEqual([]);
  });

  test("never viewed → never offline (backgrounded pane doesn't accuse)", () => {
    const h = watchHarness();
    h.feed(unwatchedBrokenView);
    vi.advanceTimersByTime(5 * GRACE);
    expect(h.changes).toEqual([]);
  });

  test("a pane that painted, then lost its view, is re-claimed then accused", () => {
    const h = retryHarness();
    h.feed(paintingView);
    vi.advanceTimersByTime(GRACE);
    expect(h.reclaimed()).toBe(0); // frames were landing: nothing to repair
    h.feed(unreachableView); // the view is gone; the painted screen is now stale
    vi.advanceTimersByTime(GRACE);
    expect(h.reclaimed()).toBe(1); // silent re-claim, no accusation yet
    expect(h.changes).toEqual([]);
    vi.advanceTimersByTime(GRACE);
    expect(h.reclaimed()).toBe(2);
    expect(h.changes).toEqual([]);
    vi.advanceTimersByTime(GRACE);
    expect(h.changes).toEqual([true]); // budget spent → offline
    expect(h.reclaimed()).toBe(2);
  });

  test("a quiet pane with a healthy view is never re-claimed, however long", () => {
    const h = retryHarness();
    h.feed(paintingView);
    vi.advanceTimersByTime(GRACE);
    h.feed(quietHealthyView); // the shell simply has nothing left to print
    vi.advanceTimersByTime(100 * GRACE);
    expect(h.reclaimed()).toBe(0);
    expect(h.changes).toEqual([]);
  });

  test("a frame clears offline immediately even while the view reads detached", () => {
    const h = watchHarness();
    h.feed(unreachableView);
    vi.advanceTimersByTime(GRACE);
    expect(h.changes).toEqual([true]);
    h.feed(paintingDetachedView); // paint beats the status it contradicts
    expect(h.changes).toEqual([true, false]);
  });

  test("offline then the view comes back → clears", () => {
    const h = watchHarness();
    h.feed(unreachableView);
    vi.advanceTimersByTime(GRACE);
    expect(h.changes).toEqual([true]);
    h.feed(paintingView); // re-claim succeeded / late replay
    expect(h.changes).toEqual([true, false]);
  });

  test("offline then pane un-viewed → clears (overlay hides on nav away)", () => {
    const h = watchHarness();
    h.feed(unreachableView);
    vi.advanceTimersByTime(GRACE);
    h.feed(unwatchedBrokenView);
    expect(h.changes).toEqual([true, false]);
  });

  test("a painting pane never arms", () => {
    const h = watchHarness();
    h.feed(paintingView);
    vi.advanceTimersByTime(5 * GRACE);
    expect(h.changes).toEqual([]);
  });

  test("repeated identical updates don't restart the grace or double-fire", () => {
    const h = watchHarness();
    h.feed(unreachableView); // arm at t=0
    vi.advanceTimersByTime(1500);
    h.feed(unreachableView); // must NOT restart the timer
    vi.advanceTimersByTime(1500); // total 3000 since first arm
    expect(h.changes).toEqual([true]); // fired once, on the original schedule
  });

  test("dispose cancels a pending accusation", () => {
    const h = watchHarness();
    h.feed(unreachableView);
    h.dispose();
    vi.advanceTimersByTime(5 * GRACE);
    expect(h.changes).toEqual([]);
  });

  test("re-claims silently on each grace, then declares offline", () => {
    const h = retryHarness();
    h.feed(unreachableView);
    vi.advanceTimersByTime(GRACE);
    expect(h.reclaimed()).toBe(1);
    expect(h.changes).toEqual([]); // silent re-claim, no accusation
    vi.advanceTimersByTime(GRACE);
    expect(h.reclaimed()).toBe(2);
    expect(h.changes).toEqual([]);
    vi.advanceTimersByTime(GRACE);
    expect(h.changes).toEqual([true]); // budget spent → offline
    expect(h.reclaimed()).toBe(2); // no re-claim fired on the final expiry
  });

  test("a frame after a silent re-claim heals without ever going offline", () => {
    const h = retryHarness();
    h.feed(unreachableView);
    vi.advanceTimersByTime(GRACE);
    expect(h.reclaimed()).toBe(1);
    h.feed(paintingView); // the re-claim's snapshot landed
    vi.advanceTimersByTime(5 * GRACE);
    expect(h.changes).toEqual([]);
  });

  test("a view that comes back mid-ladder restores the full re-claim budget", () => {
    const h = retryHarness();
    h.feed(unreachableView);
    vi.advanceTimersByTime(GRACE);
    expect(h.reclaimed()).toBe(1);
    h.feed(paintingView); // healed, budget refreshed
    h.feed(unreachableView); // broke again → a fresh ladder, not the tail of one
    vi.advanceTimersByTime(3 * GRACE);
    expect(h.reclaimed()).toBe(3); // 1 earlier + 2 fresh
    expect(h.changes).toEqual([true]);
  });

  test("re-claim budget resets when the pane is un-viewed", () => {
    const h = retryHarness();
    h.feed(unreachableView);
    vi.advanceTimersByTime(GRACE);
    expect(h.reclaimed()).toBe(1);
    h.feed(unwatchedBrokenView); // navigated away → clears + resets budget
    h.feed(unreachableView); // viewed again → fresh budget
    vi.advanceTimersByTime(3 * GRACE);
    expect(h.reclaimed()).toBe(3); // 1 earlier + 2 fresh
    expect(h.changes).toEqual([true]);
  });

  test("dispose mid-ladder cancels further re-claims and the accusation", () => {
    const h = retryHarness();
    h.feed(unreachableView);
    vi.advanceTimersByTime(GRACE);
    expect(h.reclaimed()).toBe(1);
    h.dispose();
    vi.advanceTimersByTime(5 * GRACE);
    expect(h.reclaimed()).toBe(1);
    expect(h.changes).toEqual([]);
  });

  test("retries=0 with onRetry → offline at first expiry, zero re-claims", () => {
    const h = retryHarness(0);
    h.feed(unreachableView);
    vi.advanceTimersByTime(GRACE);
    expect(h.changes).toEqual([true]);
    expect(h.reclaimed()).toBe(0);
  });

  // CellTerminal derives `painted` from FRAME_ACTIVITY_WINDOW_MS and `detached`
  // from DETACHED_GRACE_MS. A freshness window that outlived the detached grace
  // would still read "painted" at the moment a view first reads detached — the
  // one edge that arms the re-claim — and the pane would freeze with nothing
  // left to repair it.
  test("frame freshness expires before a view can read detached", () => {
    expect(FRAME_ACTIVITY_WINDOW_MS).toBeLessThan(DETACHED_GRACE_MS);
  });
});
