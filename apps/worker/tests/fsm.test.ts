// R5.3 channel FSM invariant tests.
// fast-check properties:
//   - spawned → closed without attach: only valid via direct spawned→close
//   - closed is terminal
//   - every closure emits exactly one "closed" event

import { describe, test, expect } from "bun:test";
import fc from "fast-check";
import { FsmChannel, type FsmEvent, type ChannelState } from "../src/fsm.ts";

// ─── helpers ──────────────────────────────────────────────────────────

function makeCollector(): { events: Array<{ from: ChannelState; to: ChannelState; event: FsmEvent }>; fsm: FsmChannel } {
  const events: Array<{ from: ChannelState; to: ChannelState; event: FsmEvent }> = [];
  const fsm = new FsmChannel((from, to, event) => events.push({ from, to, event }));
  return { events, fsm };
}

const ALL_EVENTS: FsmEvent[] = [
  { kind: "attach" },
  { kind: "detach" },
  { kind: "close", exitCode: 0 },
];

// Arbitrary sequence of events (not necessarily valid transitions).
const eventArb = fc.array(
  fc.oneof(
    fc.constant<FsmEvent>({ kind: "attach" }),
    fc.constant<FsmEvent>({ kind: "detach" }),
    fc.constant<FsmEvent>({ kind: "close", exitCode: 0 }),
  ),
  { minLength: 1, maxLength: 20 },
);

// ─── unit tests ───────────────────────────────────────────────────────

describe("FsmChannel", () => {
  test("initial state is spawned", () => {
    const { fsm } = makeCollector();
    expect(fsm.state).toBe("spawned");
  });

  test("attach from spawned → attached", () => {
    const { fsm } = makeCollector();
    const r = fsm.send({ kind: "attach" });
    expect(r.ok).toBe(true);
    if (r.ok) {
      expect(r.to).toBe("attached");
    }
    expect(fsm.state).toBe("attached");
  });

  test("closed is terminal — send after close is rejected", () => {
    const { fsm } = makeCollector();
    fsm.send({ kind: "attach" });
    fsm.send({ kind: "close", exitCode: null });
    expect(fsm.state).toBe("closed");
    const r = fsm.send({ kind: "attach" });
    expect(r.ok).toBe(false);
    if (!r.ok) expect(r.reason).toContain("terminal");
    expect(fsm.state).toBe("closed"); // unchanged
  });

  test("spawned → closed direct (keeper died before attach)", () => {
    const { fsm } = makeCollector();
    const r = fsm.send({ kind: "close", exitCode: 1 });
    expect(r.ok).toBe(true);
    expect(fsm.state).toBe("closed");
  });

  test("full lifecycle: spawned → attached → spawned → attached → closed", () => {
    const { fsm, events } = makeCollector();
    fsm.send({ kind: "attach" });
    fsm.send({ kind: "detach" });
    fsm.send({ kind: "attach" });
    fsm.send({ kind: "close", exitCode: 0 });
    expect(fsm.state).toBe("closed");
    expect(events.filter((e) => e.to === "closed")).toHaveLength(1);
  });

  test("detach from spawned is rejected", () => {
    const { fsm } = makeCollector();
    const r = fsm.send({ kind: "detach" });
    expect(r.ok).toBe(false);
    expect(fsm.state).toBe("spawned");
  });
});

// ─── property tests ───────────────────────────────────────────────────

describe("FsmChannel properties", () => {
  test("P1: closed is terminal — no valid transition out of closed", () => {
    fc.assert(fc.property(eventArb, (events) => {
      const { fsm } = makeCollector();
      // Drive FSM through all events.
      for (const e of events) fsm.send(e);
      if (fsm.state !== "closed") return true; // not yet closed, skip assertion
      // Any additional event must be rejected.
      for (const e of ALL_EVENTS) {
        const r = fsm.send(e);
        if (r.ok) return false; // violated: escaped closed
      }
      return true;
    }), { numRuns: 200 });
  });

  test("P2: exactly one closed event emitted per session lifetime", () => {
    fc.assert(fc.property(eventArb, (events) => {
      const { fsm, events: emitted } = makeCollector();
      for (const e of events) fsm.send(e);
      const closedCount = emitted.filter((e) => e.to === "closed").length;
      // Either 0 (not yet closed) or exactly 1.
      return closedCount <= 1;
    }), { numRuns: 200 });
  });

  test("P3: attached is reachable only through attach", () => {
    fc.assert(fc.property(eventArb, (events) => {
      const { fsm, events: emitted } = makeCollector();
      for (const e of events) fsm.send(e);
      if (fsm.state !== "attached") return true;
      return emitted.some((transition) =>
        transition.event.kind === "attach" && transition.to === "attached"
      );
    }), { numRuns: 200 });
  });
});
