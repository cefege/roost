// Stuck-terminal watcher — the state machine behind MainPane's "don't strand
// the user on a black pane when bootstrap can't complete" escape card. The
// dead-route safety net only bounces a blank terminal route Home once the
// session list has hydrated; when the coord is unreachable (a fresh load mid
// coord-restart) or the browser is unpaired, hydration never lands and the user
// is stranded. This watcher flips a flag so MainPane shows an actionable card
// instead — debounced so a healthy load's fast hydration never flashes it.
//
// We test createStuckTerminalWatcher (the pure state machine) directly rather
// than installStuckTerminalWatcher: Solid resolves to its SSR build under
// `bun test`, where createEffect never fires, so the reactive wrapper is inert.
// Driving `evaluate()` by hand simulates each reactive re-run; fake timers drive
// the debounce deterministically. Mirrors deadRouteSafetyNet.test.ts.

import { expect, test, describe, beforeEach, afterEach, vi } from "bun:test";
import { createStuckTerminalWatcher, type StuckKind } from "../src/lib/stuckTerminal.ts";

const GRACE = 600;

// Mutable stub state + an emit log. onChange dedups (only real transitions
// land), so `emitted` is the exact sequence of display-state changes MainPane's
// signal would see.
function harness() {
  const state = {
    onRoute: true,
    hasOpen: false,
    hydrated: true,
    unauthorized: false,
  };
  const emitted: (StuckKind | null)[] = [];
  const w = createStuckTerminalWatcher({
    onTerminalRoute: () => state.onRoute,
    hasOpenSession: () => state.hasOpen,
    hydrated: () => state.hydrated,
    unauthorized: () => state.unauthorized,
    onChange: (k) => emitted.push(k),
    graceMs: GRACE,
  });
  return { state, emitted, evaluate: w.evaluate, dispose: w.dispose };
}

describe("createStuckTerminalWatcher", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  test("healthy load: unhydrated blip resolves within grace → never flashes", () => {
    const h = harness();
    h.state.hydrated = false; // bootstrapping onto /s/<live>
    h.evaluate(); // stuck (connecting) → schedule show
    vi.advanceTimersByTime(300); // partway through grace
    h.state.hydrated = true;
    h.state.hasOpen = true; // hydrated + session resolved
    h.evaluate(); // not stuck → cancels the pending show
    vi.advanceTimersByTime(5 * GRACE);
    expect(h.emitted).toEqual([]);
  });

  test("coord unreachable: unhydrated + no session persists → shows connecting after grace", () => {
    const h = harness();
    h.state.hydrated = false;
    h.evaluate();
    vi.advanceTimersByTime(GRACE);
    expect(h.emitted).toEqual(["connecting"]);
  });

  test("unpaired wins over unhydrated (both true on an untrusted browser)", () => {
    const h = harness();
    h.state.unauthorized = true;
    h.state.hydrated = false;
    h.evaluate();
    vi.advanceTimersByTime(GRACE);
    expect(h.emitted).toEqual(["unpaired"]);
  });

  test("recovery hides the card (session arrives after it showed)", () => {
    const h = harness();
    h.state.hydrated = false;
    h.evaluate();
    vi.advanceTimersByTime(GRACE); // connecting shown
    h.state.hasOpen = true; // session arrived
    h.evaluate();
    expect(h.emitted).toEqual(["connecting", null]);
  });

  test("recovers exactly as the timer fires → re-check emits nothing", () => {
    const h = harness();
    h.state.hydrated = false;
    h.evaluate(); // schedule
    h.state.hasOpen = true; // recovered but NO further evaluate() to clear the timer
    vi.advanceTimersByTime(GRACE); // fires; re-check sees a session → stays hidden
    expect(h.emitted).toEqual([]);
  });

  test("off a terminal route → never shows", () => {
    const h = harness();
    h.state.onRoute = false;
    h.state.hydrated = false;
    h.evaluate();
    vi.advanceTimersByTime(5 * GRACE);
    expect(h.emitted).toEqual([]);
  });

  test("hydrated + authorized + no session (durably gone) → never shows (safety net owns it)", () => {
    const h = harness();
    h.state.hydrated = true;
    h.state.unauthorized = false;
    h.state.hasOpen = false;
    h.evaluate();
    vi.advanceTimersByTime(5 * GRACE);
    expect(h.emitted).toEqual([]);
  });

  test("kind flips live while shown → updates immediately, no re-debounce", () => {
    const h = harness();
    h.state.hydrated = false;
    h.evaluate();
    vi.advanceTimersByTime(GRACE); // connecting shown
    h.state.unauthorized = true; // now also unpaired
    h.evaluate(); // already showing → swap kind at once
    expect(h.emitted).toEqual(["connecting", "unpaired"]);
  });
});
