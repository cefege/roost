// The sweep runs on a 1s interval and calls two foreign functions per tick:
// the live-view expiry notification (which closes the owning socket) and the
// per-session recompute that owns every session's PTY geometry. A throw from
// either used to escape the interval callback, so one failing session froze
// geometry for the whole process. Constructs TerminalViewRegistry directly
// because the hub wires recompute to its own controller, which cannot fail.
import { afterEach, describe, expect, test } from "bun:test";
import { TERMINAL_VIEW_LEASE_MS } from "@roost/protocol/viewport";
import { TerminalScreenHub } from "../src/connect/terminal-screen-hub.ts";
import { TerminalViewRegistry } from "@roost/protocol/terminal-view";
import {
  OTHER_SESSION,
  SESSION,
  TestSink,
  VIEW_A,
  VIEW_B,
  viewCommand,
} from "./terminal-view-hub-harness.ts";

const liveScreens: TerminalScreenHub[] = [];

afterEach(() => {
  for (const screen of liveScreens.splice(0)) screen.dispose();
});

interface Poison {
  recompute?: string;
  expiry?: string;
}

interface Harness {
  registry: TerminalViewRegistry;
  clock: { value: number };
  recomputed: string[];
  expiries: string[];
  /** Armed AFTER attach: the command path recomputes on admit, and only the
   *  sweep's boundary is under test here. */
  poison: Poison;
}

function makeRegistry(): Harness {
  const poison: Poison = {};
  const clock = { value: 1_000 };
  const recomputed: string[] = [];
  const expiries: string[] = [];
  const screen = new TerminalScreenHub({
    requestSnapshot: () => undefined,
    requestFreshStream: () => undefined,
    now: () => clock.value,
  });
  liveScreens.push(screen);
  const registry = new TerminalViewRegistry({
    screen,
    now: () => clock.value,
    streamState: () => null,
    recompute: (sessionId) => {
      if (sessionId === poison.recompute) throw new Error("recompute failed");
      recomputed.push(sessionId);
      return true;
    },
    redrive: () => undefined,
    onLiveViewExpired: (_socketId, _viewId, sessionId) => {
      if (sessionId === poison.expiry) throw new Error("socket teardown failed");
      expiries.push(sessionId);
    },
  });
  return { registry, clock, recomputed, expiries, poison };
}

function attach(registry: TerminalViewRegistry): void {
  for (const socketId of ["socket-a", "socket-b"]) {
    registry.registerSocket({
      socketId,
      viewerKey: `viewer-${socketId}`,
      callerFingerprint: `fingerprint-${socketId}`,
      allowsSession: () => true,
      sink: new TestSink(),
    });
  }
  registry.handleViewCommand("socket-a", viewCommand(VIEW_A, 1n, {
    sessionId: SESSION,
    cols: 80,
    rows: 24,
  }));
  registry.handleViewCommand("socket-b", viewCommand(VIEW_B, 1n, {
    sessionId: OTHER_SESSION,
    cols: 120,
    rows: 50,
  }));
}

describe("terminal view sweep failure boundary", () => {
  test("a failing session's recompute cannot starve the other sessions", () => {
    const { registry, clock, recomputed, poison } = makeRegistry();
    attach(registry);
    poison.recompute = SESSION;
    recomputed.length = 0;

    clock.value += TERMINAL_VIEW_LEASE_MS;
    expect(() => registry.sweep()).not.toThrow();

    // Isolation, not repair: the poisoned session is skipped and every other
    // affected session still re-minimizes on the same tick.
    expect(recomputed).toEqual([OTHER_SESSION]);
    expect(registry.viewStats(SESSION)).toEqual({ activeViews: 0, parkedViews: 0 });
    expect(registry.viewStats(OTHER_SESSION)).toEqual({ activeViews: 0, parkedViews: 0 });
  });

  test("a failing expiry notification still reaps and recomputes every record", () => {
    const { registry, clock, recomputed, expiries, poison } = makeRegistry();
    attach(registry);
    poison.expiry = SESSION;
    recomputed.length = 0;

    clock.value += TERMINAL_VIEW_LEASE_MS;
    expect(() => registry.sweep()).not.toThrow();

    expect(expiries).toEqual([OTHER_SESSION]);
    expect([...recomputed].sort()).toEqual([SESSION, OTHER_SESSION].sort());
    expect(registry.geometries(SESSION)).toEqual({ live: [], retained: 0 });
  });

  test("the sweep keeps working on later ticks after a failure", () => {
    const { registry, clock, recomputed, poison } = makeRegistry();
    attach(registry);
    poison.recompute = SESSION;

    clock.value += TERMINAL_VIEW_LEASE_MS;
    expect(() => registry.sweep()).not.toThrow();

    registry.registerSocket({
      socketId: "socket-c",
      viewerKey: "viewer-socket-c",
      callerFingerprint: "fingerprint-socket-c",
      allowsSession: () => true,
      sink: new TestSink(),
    });
    registry.handleViewCommand("socket-c", viewCommand(VIEW_A, 1n, {
      sessionId: OTHER_SESSION,
      cols: 90,
      rows: 30,
    }));
    recomputed.length = 0;

    clock.value += TERMINAL_VIEW_LEASE_MS;
    expect(() => registry.sweep()).not.toThrow();
    expect(recomputed).toEqual([OTHER_SESSION]);
  });
});
