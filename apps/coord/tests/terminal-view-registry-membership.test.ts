// Which viewer records constrain the PTY, and when they stop. Covers the
// smallest-common-denominator minimum across DISTINCT sockets, the park grace
// that releases a dead viewer's geometry, the hold rule that keeps a solo
// viewer's blip from re-minting the stream, lease-expired-but-unswept records,
// and the re-entrant sweep where one socket's expiry parks its other sessions.
import { afterEach, describe, expect, test } from "bun:test";
import { TerminalViewStatus } from "@roost/shared/proto/sync_pb";
import {
  TERMINAL_VIEW_LEASE_MS,
  TERMINAL_VIEW_PARK_GRACE_MS,
  TERMINAL_VIEW_SWEEP_MS,
} from "@roost/shared/viewport";
import { globalPresenceBus } from "../src/buses.ts";
import {
  OTHER_SESSION,
  SESSION,
  VIEW_A,
  VIEW_B,
  disposeHubs,
  makeHarness,
  register,
  settle,
  statesFor,
  sweep,
  viewCommand,
} from "./terminal-view-hub-harness.ts";

afterEach(disposeHubs);

describe("terminal view geometry membership", () => {
  test("minimizes each axis across two distinct viewer sockets", async () => {
    const { hub, sent } = makeHarness();
    const desktop = register(hub, "socket-a", "viewer-a", "fingerprint-a");
    const phone = register(hub, "socket-b", "viewer-b", "fingerprint-b");

    hub.handleViewCommand("socket-a", viewCommand(VIEW_A, 1n, { cols: 80, rows: 50 }));
    hub.handleViewCommand("socket-b", viewCommand(VIEW_A, 1n, { cols: 120, rows: 24 }));
    await settle();

    expect(hub.snapshot(SESSION)).toMatchObject({
      activeViews: 2,
      parkedViews: 0,
      effective: { cols: 80, rows: 24 },
    });
    expect(sent.at(-1)).toMatchObject({ enabled: true, cols: 80, rows: 24 });
    for (const sink of [desktop, phone]) {
      expect(statesFor(sink, VIEW_A).at(-1)).toMatchObject({
        status: TerminalViewStatus.ACCEPTED,
        effectiveCols: 80,
        effectiveRows: 24,
      });
    }
  });

  test("re-widens to the survivor within the park grace when the smaller viewer's socket dies", async () => {
    const clock = { value: 0 };
    const { hub, sent } = makeHarness({ clock });
    register(hub, "socket-a", "viewer-a", "fingerprint-a");
    register(hub, "socket-b", "viewer-b", "fingerprint-b");
    hub.handleViewCommand("socket-a", viewCommand(VIEW_A, 1n, { cols: 80, rows: 24 }));
    hub.handleViewCommand("socket-b", viewCommand(VIEW_A, 1n, { cols: 120, rows: 50 }));
    await settle();
    expect(hub.snapshot(SESSION)?.effective).toEqual({ cols: 80, rows: 24 });
    const transitions = sent.length;

    hub.closeSocket("socket-a");
    clock.value = TERMINAL_VIEW_PARK_GRACE_MS - 1;
    sweep(hub);
    await settle();
    expect(hub.snapshot(SESSION)?.effective).toEqual({ cols: 80, rows: 24 });
    expect(sent).toHaveLength(transitions);

    clock.value = TERMINAL_VIEW_PARK_GRACE_MS;
    sweep(hub);
    await settle();
    expect(hub.snapshot(SESSION)).toMatchObject({
      activeViews: 1,
      parkedViews: 1,
      effective: { cols: 120, rows: 50 },
    });
    expect(sent.at(-1)).toMatchObject({ enabled: true, cols: 120, rows: 50 });
    // Grace-bounded, not lease-bounded: the dead viewer still holds its claim.
    expect(clock.value).toBeLessThan(TERMINAL_VIEW_LEASE_MS);
  });

  test("holds a solo viewer's geometry across a socket blip", async () => {
    const clock = { value: 0 };
    const { hub, sent } = makeHarness({ clock });
    register(hub);
    hub.handleViewCommand("socket-a", viewCommand(VIEW_A, 1n, { cols: 96, rows: 42 }));
    await settle();
    const transitions = sent.length;
    const streamId = hub.snapshot(SESSION)!.streamId;

    hub.closeSocket("socket-a");
    for (const at of [
      0,
      TERMINAL_VIEW_PARK_GRACE_MS - 1,
      TERMINAL_VIEW_PARK_GRACE_MS,
      TERMINAL_VIEW_PARK_GRACE_MS + TERMINAL_VIEW_SWEEP_MS,
      TERMINAL_VIEW_LEASE_MS - 1,
    ]) {
      clock.value = at;
      sweep(hub);
      await settle();
      expect(hub.snapshot(SESSION)).toMatchObject({
        activeViews: 0,
        parkedViews: 1,
        effective: { cols: 96, rows: 42 },
        streamId,
      });
      expect(sent).toHaveLength(transitions);
    }

    // Losing membership entirely — not parking — is what disables the stream.
    clock.value = TERMINAL_VIEW_LEASE_MS;
    sweep(hub);
    await settle();
    expect(hub.snapshot(SESSION)).toMatchObject({ activeViews: 0, parkedViews: 0, effective: null });
    expect(sent.at(-1)).toMatchObject({ enabled: false });
  });

  test("ignores a lease-expired record that no sweep has reaped yet", async () => {
    const clock = { value: 0 };
    const { hub, sent } = makeHarness({ clock });
    register(hub, "socket-a", "viewer-a", "fingerprint-a");
    register(hub, "socket-b", "viewer-b", "fingerprint-b");
    hub.handleViewCommand("socket-a", viewCommand(VIEW_A, 1n, { cols: 80, rows: 24 }));
    await settle();

    clock.value = TERMINAL_VIEW_LEASE_MS;
    hub.handleViewCommand("socket-b", viewCommand(VIEW_A, 1n, { cols: 120, rows: 50 }));
    await settle();

    expect(hub.snapshot(SESSION)?.effective).toEqual({ cols: 120, rows: 50 });
    expect(sent.at(-1)).toMatchObject({ enabled: true, cols: 120, rows: 50 });
  });

  test("drops a lapsed park exactly once, not on every sweep tick", async () => {
    const clock = { value: 0 };
    const { hub, sent } = makeHarness({ clock });
    register(hub, "socket-a", "viewer-a", "fingerprint-a");
    register(hub, "socket-b", "viewer-b", "fingerprint-b");
    hub.handleViewCommand("socket-a", viewCommand(VIEW_A, 1n, { cols: 80, rows: 24 }));
    hub.handleViewCommand("socket-b", viewCommand(VIEW_A, 1n, { cols: 120, rows: 50 }));
    await settle();
    const transitions = sent.length;

    let presencePublishes = 0;
    const unsubscribe = globalPresenceBus.subscribe((message) => {
      if (message.session_id === SESSION) presencePublishes += 1;
    });
    hub.closeSocket("socket-a");
    for (let tick = 1; tick <= 8; tick += 1) {
      clock.value = tick * TERMINAL_VIEW_SWEEP_MS;
      sweep(hub);
      await settle();
    }
    unsubscribe();

    expect(presencePublishes).toBe(1);
    expect(sent).toHaveLength(transitions + 1);
    expect(hub.snapshot(SESSION)?.effective).toEqual({ cols: 120, rows: 50 });
  });

  test("renews the lease for a heartbeat whose drifted geometry is rejected", async () => {
    const clock = { value: 0 };
    const { hub } = makeHarness({ clock });
    const sink = register(hub);
    const expired: string[] = [];
    hub.setOnLiveViewExpired((socketId) => expired.push(socketId));
    hub.handleViewCommand("socket-a", viewCommand(VIEW_A, 1n, { cols: 80, rows: 24 }));
    await settle();

    clock.value = TERMINAL_VIEW_LEASE_MS - 1;
    hub.handleViewCommand("socket-a", viewCommand(VIEW_A, 1n, { cols: 81, rows: 24 }));
    await settle();
    expect(statesFor(sink, VIEW_A).at(-1)).toMatchObject({
      status: TerminalViewStatus.REJECTED,
      reason: "terminal view revision conflicts",
    });
    expect(hub.snapshot(SESSION)?.effective).toEqual({ cols: 80, rows: 24 });

    clock.value = TERMINAL_VIEW_LEASE_MS;
    sweep(hub);
    await settle();
    expect(hub.snapshot(SESSION)).toMatchObject({ activeViews: 1, effective: { cols: 80, rows: 24 } });
    expect(expired).toEqual([]);

    clock.value = TERMINAL_VIEW_LEASE_MS * 2;
    sweep(hub);
    await settle();
    expect(expired).toEqual(["socket-a"]);
  });

  test("re-minimizes the sibling session a re-entrant sweep close parked", async () => {
    const clock = { value: 0 };
    const { hub, sent } = makeHarness({ clock });
    register(hub, "socket-a", "viewer-a", "fingerprint-a");
    register(hub, "socket-b", "viewer-b", "fingerprint-b");
    // sync-ws closes the whole socket when one of its views expires.
    hub.setOnLiveViewExpired((socketId) => hub.closeSocket(socketId));
    hub.handleViewCommand("socket-a", viewCommand(VIEW_A, 1n, { cols: 80, rows: 24 }));
    hub.handleViewCommand("socket-a", viewCommand(VIEW_B, 1n, {
      sessionId: OTHER_SESSION,
      cols: 80,
      rows: 24,
    }));
    hub.handleViewCommand("socket-b", viewCommand(VIEW_B, 1n, {
      sessionId: OTHER_SESSION,
      cols: 120,
      rows: 50,
    }));
    await settle();
    expect(hub.snapshot(OTHER_SESSION)?.effective).toEqual({ cols: 80, rows: 24 });

    // Both surviving views heartbeat; only the SESSION view goes silent.
    clock.value = TERMINAL_VIEW_LEASE_MS / 2;
    hub.handleViewCommand("socket-a", viewCommand(VIEW_B, 1n, {
      sessionId: OTHER_SESSION,
      cols: 80,
      rows: 24,
    }));
    hub.handleViewCommand("socket-b", viewCommand(VIEW_B, 1n, {
      sessionId: OTHER_SESSION,
      cols: 120,
      rows: 50,
    }));
    await settle();

    clock.value = TERMINAL_VIEW_LEASE_MS;
    sweep(hub);
    await settle();
    const parkedAt = clock.value;
    expect(hub.snapshot(SESSION)).toMatchObject({ activeViews: 0, parkedViews: 0, effective: null });
    expect(hub.snapshot(OTHER_SESSION)).toMatchObject({
      activeViews: 1,
      parkedViews: 1,
      effective: { cols: 80, rows: 24 },
    });

    clock.value = parkedAt + TERMINAL_VIEW_PARK_GRACE_MS;
    sweep(hub);
    await settle();
    expect(hub.snapshot(OTHER_SESSION)?.effective).toEqual({ cols: 120, rows: 50 });
    expect(sent.at(-1)).toMatchObject({ enabled: true, cols: 120, rows: 50 });
  });

  test("a throwing expiry notification cannot abandon the sweep", async () => {
    const { hub, clock, sent } = makeHarness();
    const origin = clock.value;
    register(hub, "socket-a", "viewer-a", "fingerprint-a");
    register(hub, "socket-b", "viewer-b", "fingerprint-b");
    // The production handler closes the owning socket synchronously. A throw
    // there used to abort the whole tick, leaving every later session holding a
    // dead viewer's geometry and killing the interval that owns the recompute.
    hub.setOnLiveViewExpired(() => {
      throw new Error("sync socket teardown failed");
    });

    hub.handleViewCommand("socket-a", viewCommand(VIEW_A, 1n, { cols: 80, rows: 24 }));
    hub.handleViewCommand("socket-b", viewCommand(VIEW_B, 1n, {
      sessionId: OTHER_SESSION,
      cols: 120,
      rows: 50,
    }));
    await settle();

    // socket-b keeps its lease; only the SESSION view expires.
    clock.value = origin + TERMINAL_VIEW_LEASE_MS / 2;
    hub.handleViewCommand("socket-b", viewCommand(VIEW_B, 1n, {
      sessionId: OTHER_SESSION,
      cols: 120,
      rows: 50,
    }));
    await settle();

    clock.value = origin + TERMINAL_VIEW_LEASE_MS;
    expect(() => sweep(hub)).not.toThrow();
    await settle();

    expect(hub.snapshot(SESSION)).toMatchObject({
      activeViews: 0,
      parkedViews: 0,
      effective: null,
    });
    expect(sent.at(-1)).toMatchObject({ sessionId: SESSION, enabled: false });
    expect(hub.snapshot(OTHER_SESSION)).toMatchObject({
      activeViews: 1,
      effective: { cols: 120, rows: 50 },
    });

    // The interval's owner survives: a later tick still reaps and recomputes.
    clock.value = origin + TERMINAL_VIEW_LEASE_MS * 2;
    expect(() => sweep(hub)).not.toThrow();
    await settle();
    expect(hub.snapshot(OTHER_SESSION)).toMatchObject({
      activeViews: 0,
      effective: null,
    });
  });
});
