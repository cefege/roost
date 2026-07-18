// Dead-route safety net — the state machine behind MainPane's "don't strand
// the user on a blank terminal pane" redirect. The reported bug: a LIVE
// session whose URL-resolution blips to null for a tick was bounced to Home
// synchronously. The fix routes EVERY null through a grace window + re-check,
// so a resolution that recovers never bounces; only a durably-gone terminal
// navigates — to a sibling in the same folder, else Home.
//
// We test createDeadRouteSafetyNet (the pure state machine) directly rather
// than installDeadRouteSafetyNet: Solid resolves to its SSR build under
// `bun test`, where createEffect never fires, so the reactive wrapper is inert
// here. Driving `evaluate()` by hand = simulating each reactive re-run. Fake
// timers drive the grace window deterministically (no wall-clock waits).

import { expect, test, describe, beforeEach, afterEach, vi } from "bun:test";
import { asWorkerFp, asSessionId, asChannelId } from "@roost/shared/wire";
import type { Session } from "@roost/shared/wire";
import { createDeadRouteSafetyNet } from "../src/lib/deadRouteSafetyNet.ts";

const FP = asWorkerFp("aa".repeat(32));
const GRACE = 2500;

function sess(id: string, created_at = 1000): Session {
  return {
    id: asSessionId(id),
    worker_fp: FP,
    channel: asChannelId(1),
    kind: "shell",
    cwd: "/Users/you/roost",
    spawn_cwd: "/Users/you/roost",
    workspace_id: null,
    status: "open",
    agent: null,
    created_at,
    closed_at: null,
    custom_title: null,
  } as Session;
}

const VIEWED = sess("00000000-0000-4000-8000-000000000001");
const SIBLING = sess("00000000-0000-4000-8000-000000000002", 2000);

interface Bounce {
  target: string;
  reason: string;
  sid: string;
}

// Build the state machine over mutable stub state. `bounceTarget`/`onBounce`
// mirror MainPane's real wiring (sibling-in-folder → "/s/<id>", else "/"; reason
// derived from whether a session was ever seen open) so the test asserts the
// exact hrefs and diag reasons production emits.
function harness() {
  const state = {
    open: null as Session | null,
    onRoute: true,
    hydrated: true,
    sibling: null as Session | null,
  };
  const navigated: string[] = [];
  const bounces: Bounce[] = [];
  const net = createDeadRouteSafetyNet({
    onTerminalRoute: () => state.onRoute,
    activeOpenSession: () => state.open,
    hydrated: () => state.hydrated,
    navigate: (href) => navigated.push(href),
    bounceTarget: (lastOpen) =>
      lastOpen && state.sibling ? `/s/${state.sibling.id}` : "/",
    onBounce: (target, lastOpen) =>
      bounces.push({
        target,
        reason: lastOpen ? "gone" : "stale-deeplink",
        sid: lastOpen?.id ?? "",
      }),
    graceMs: GRACE,
  });
  return { state, navigated, bounces, evaluate: net.evaluate };
}

describe("createDeadRouteSafetyNet", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  test("transient: open→null→open within grace → NO bounce (the reported bug)", () => {
    const h = harness();
    h.state.open = VIEWED;
    h.evaluate(); // live → remembered, no timer
    h.state.open = null;
    h.evaluate(); // resolution blip → schedule bounce
    vi.advanceTimersByTime(1000); // partway through grace
    h.state.open = VIEWED;
    h.evaluate(); // recovered → cancels the pending bounce
    vi.advanceTimersByTime(5 * GRACE); // well past grace
    expect(h.navigated).toEqual([]);
    expect(h.bounces).toEqual([]);
  });

  test("transient: recovers exactly as the timer fires → re-check cancels the bounce", () => {
    const h = harness();
    h.state.open = VIEWED;
    h.evaluate();
    h.state.open = null;
    h.evaluate(); // schedule
    h.state.open = VIEWED; // recovered but NO further evaluate() to clear the timer
    vi.advanceTimersByTime(GRACE); // timer fires; its re-check sees open again
    expect(h.navigated).toEqual([]);
    expect(h.bounces).toEqual([]);
  });

  test("durable gone, sibling present → navigates to the sibling, reason=gone", () => {
    const h = harness();
    h.state.sibling = SIBLING;
    h.state.open = VIEWED;
    h.evaluate();
    h.state.open = null;
    h.evaluate(); // schedule
    vi.advanceTimersByTime(GRACE); // fire; still gone
    expect(h.navigated).toEqual([`/s/${SIBLING.id}`]);
    expect(h.bounces).toEqual([
      { target: `/s/${SIBLING.id}`, reason: "gone", sid: VIEWED.id as string },
    ]);
  });

  test("durable gone, no sibling → navigates Home", () => {
    const h = harness();
    h.state.sibling = null;
    h.state.open = VIEWED;
    h.evaluate();
    h.state.open = null;
    h.evaluate();
    vi.advanceTimersByTime(GRACE);
    expect(h.navigated).toEqual(["/"]);
    expect(h.bounces[0]?.reason).toBe("gone");
  });

  test("pre-hydration: null before hydration → wait, never bounce", () => {
    const h = harness();
    h.state.hydrated = false;
    h.state.open = null;
    h.evaluate(); // gated on hydration → no timer
    vi.advanceTimersByTime(5 * GRACE);
    expect(h.navigated).toEqual([]);
  });

  test("stale deep-link (never open) → Home after grace, reason=stale-deeplink", () => {
    const h = harness();
    h.state.open = null; // never seen open → lastOpen stays null
    h.evaluate(); // schedule
    vi.advanceTimersByTime(GRACE);
    expect(h.navigated).toEqual(["/"]);
    expect(h.bounces).toEqual([{ target: "/", reason: "stale-deeplink", sid: "" }]);
  });

  test("off a terminal route → never bounce", () => {
    const h = harness();
    h.state.onRoute = false;
    h.state.open = null;
    h.evaluate();
    vi.advanceTimersByTime(5 * GRACE);
    expect(h.navigated).toEqual([]);
  });
});
