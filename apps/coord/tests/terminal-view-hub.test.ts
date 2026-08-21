import { afterEach, describe, expect, test } from "bun:test";
import { TerminalViewStatus } from "@roost/shared/proto/sync_pb";
import { TERMINAL_VIEW_LEASE_MS } from "@roost/shared/viewport";
import {
  MAX_U64,
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
  terminalStates,
  uuid,
  viewCommand,
} from "./terminal-view-hub-harness.ts";

afterEach(disposeHubs);
describe("TerminalViewHub membership ownership", () => {
  test("uses independent-axis minima, skips non-min transitions, grows on removal, and disables on hide", async () => {
    const { hub, sent } = makeHarness();
    const sink = register(hub);

    hub.handleViewCommand("socket-a", viewCommand(VIEW_A, 1n, { cols: 80, rows: 40 }));
    await settle();
    expect(sent.map(({ enabled, cols, rows }) => ({ enabled, cols, rows }))).toEqual([
      { enabled: true, cols: 80, rows: 40 },
    ]);

    hub.handleViewCommand("socket-a", viewCommand(VIEW_B, 1n, { cols: 120, rows: 20 }));
    await settle();
    expect(hub.snapshot(SESSION)?.effective).toEqual({ cols: 80, rows: 20 });
    expect(sent.at(-1)).toMatchObject({ enabled: true, cols: 80, rows: 20 });
    const minimumStream = hub.snapshot(SESSION)!.streamId;
    const transitionsAtMinimum = sent.length;

    hub.handleViewCommand("socket-a", viewCommand(VIEW_B, 2n, { cols: 130, rows: 20 }));
    await settle();
    expect(sent).toHaveLength(transitionsAtMinimum);
    expect(hub.snapshot(SESSION)?.streamId).toBe(minimumStream);
    expect(statesFor(sink, VIEW_B).at(-1)).toMatchObject({
      status: TerminalViewStatus.ACCEPTED,
      effectiveCols: 80,
      effectiveRows: 20,
      streamId: minimumStream,
    });

    hub.handleViewCommand("socket-a", viewCommand(VIEW_A, 2n, {
      active: false,
      cols: 0,
      rows: 0,
    }));
    await settle();
    expect(hub.snapshot(SESSION)?.effective).toEqual({ cols: 130, rows: 20 });
    expect(sent.at(-1)).toMatchObject({ enabled: true, cols: 130, rows: 20 });
    expect(statesFor(sink, VIEW_A).at(-1)).toMatchObject({
      active: false,
      status: TerminalViewStatus.ACCEPTED,
      streamId: "",
    });

    hub.handleViewCommand("socket-a", viewCommand(VIEW_B, 3n, {
      active: false,
      cols: 0,
      rows: 0,
    }));
    await settle();
    expect(hub.snapshot(SESSION)).toMatchObject({ activeViews: 0, effective: null });
    expect(sent.at(-1)).toMatchObject({ enabled: false, cols: 0, rows: 0 });
    expect(new Set(sent.map((state) => state.streamId)).size).toBe(4);
  });

  test("parks membership across disconnect and transfers ownership only on exact replay", async () => {
    const { hub, sent } = makeHarness();
    const first = register(hub);
    const original = viewCommand(VIEW_A, 7n, { cols: 91, rows: 37 });
    hub.handleViewCommand("socket-a", original);
    await settle();
    const streamId = hub.snapshot(SESSION)!.streamId;
    const transitions = sent.length;

    hub.closeSocket("socket-a");
    expect(first.drops).toEqual([SESSION]);
    expect(hub.snapshot(SESSION)).toMatchObject({ activeViews: 1, parkedViews: 1 });

    const resumed = register(hub, "socket-b", "viewer-a", "fingerprint-a");
    hub.handleViewCommand("socket-b", viewCommand(VIEW_A, 7n, { cols: 92, rows: 37 }));
    hub.handleViewCommand("socket-b", viewCommand(VIEW_A, 8n, { cols: 91, rows: 37 }));
    hub.handleViewCommand("socket-b", original);
    await settle();

    expect(statesFor(resumed, VIEW_A).map((state) => state.status)).toEqual([
      TerminalViewStatus.REJECTED,
      TerminalViewStatus.REJECTED,
      TerminalViewStatus.ACCEPTED,
    ]);
    expect(statesFor(resumed, VIEW_A).at(-1)?.streamId).toBe(streamId);
    expect(resumed.begins).toEqual([[SESSION, streamId]]);
    expect(hub.snapshot(SESSION)).toMatchObject({ activeViews: 1, parkedViews: 0 });
    expect(sent).toHaveLength(transitions);
  });

  test("orders live and tombstoned revisions and rejects conflicts or session moves", async () => {
    const { hub } = makeHarness();
    const sink = register(hub);

    hub.handleViewCommand("socket-a", viewCommand(VIEW_A, 2n));
    hub.handleViewCommand("socket-a", viewCommand(VIEW_A, 1n));
    hub.handleViewCommand("socket-a", viewCommand(VIEW_A, 2n, { cols: 81 }));
    hub.handleViewCommand("socket-a", viewCommand(VIEW_A, 3n, {
      active: false,
      cols: 0,
      rows: 0,
    }));
    hub.handleViewCommand("socket-a", viewCommand(VIEW_A, 3n, {
      active: false,
      cols: 0,
      rows: 0,
    }));
    hub.handleViewCommand("socket-a", viewCommand(VIEW_A, 2n));
    hub.handleViewCommand("socket-a", viewCommand(VIEW_A, 3n));
    hub.handleViewCommand("socket-a", viewCommand(VIEW_A, 4n));
    hub.handleViewCommand("socket-a", viewCommand(VIEW_A, 5n, { sessionId: OTHER_SESSION }));
    await settle();

    expect(statesFor(sink, VIEW_A).map((state) => state.status)).toEqual([
      TerminalViewStatus.ACCEPTED,
      TerminalViewStatus.REJECTED,
      TerminalViewStatus.REJECTED,
      TerminalViewStatus.ACCEPTED,
      TerminalViewStatus.ACCEPTED,
      TerminalViewStatus.REJECTED,
      TerminalViewStatus.REJECTED,
      TerminalViewStatus.ACCEPTED,
      TerminalViewStatus.REJECTED,
    ]);
    expect(statesFor(sink, VIEW_A).at(-1)?.reason).toContain("cannot change sessions");
    expect(hub.snapshot(SESSION)).toMatchObject({ activeViews: 1 });
    expect(hub.snapshot(OTHER_SESSION)).toBeNull();
  });

  test("expires a lease at the exact boundary and retains its tombstone for one lease", async () => {
    const clock = { value: 0 };
    const { hub, sent } = makeHarness({ clock });
    const sink = register(hub);
    hub.handleViewCommand("socket-a", viewCommand(VIEW_A, 1n, { cols: 80, rows: 24 }));
    await settle();

    clock.value = TERMINAL_VIEW_LEASE_MS - 1;
    sweep(hub);
    expect(hub.snapshot(SESSION)).toMatchObject({ activeViews: 1 });

    clock.value = TERMINAL_VIEW_LEASE_MS;
    sweep(hub);
    await settle();
    expect(hub.snapshot(SESSION)).toMatchObject({ activeViews: 0, effective: null });
    expect(sent.at(-1)?.enabled).toBe(false);

    hub.handleViewCommand("socket-a", viewCommand(VIEW_A, 1n, { cols: 81, rows: 24 }));
    expect(statesFor(sink, VIEW_A).at(-1)?.status).toBe(TerminalViewStatus.REJECTED);

    clock.value = TERMINAL_VIEW_LEASE_MS * 2;
    sweep(hub);
    hub.handleViewCommand("socket-a", viewCommand(VIEW_A, 1n, { cols: 81, rows: 24 }));
    await settle();
    expect(statesFor(sink, VIEW_A).at(-1)?.status).toBe(TerminalViewStatus.ACCEPTED);
    expect(hub.snapshot(SESSION)?.effective).toEqual({ cols: 81, rows: 24 });
  });

  test("enforces command bounds without creating worker ownership", async () => {
    const { hub, sent } = makeHarness();
    const sink = register(hub);
    const unbound = register(hub, "socket-unbound", null, "fingerprint-unbound");
    const invalid = [
      viewCommand("not-a-uuid", 1n),
      viewCommand(VIEW_A, 1n, { sessionId: "not-a-uuid" }),
      viewCommand(VIEW_A, 0n),
      viewCommand(VIEW_A, MAX_U64 + 1n),
      viewCommand(VIEW_A, 1n, { cols: 0, rows: 24 }),
      viewCommand(VIEW_A, 1n, { cols: 80, rows: 257 }),
      viewCommand(VIEW_A, 1n, { active: false, cols: -1, rows: 0 }),
      viewCommand(VIEW_A, 1n, { active: false, cols: 0, rows: 257 }),
    ];
    for (const command of invalid) hub.handleViewCommand("socket-a", command);
    hub.handleViewCommand("socket-unbound", viewCommand(VIEW_A, 1n));
    await settle();

    expect(terminalStates(sink)).toHaveLength(invalid.length);
    expect(terminalStates(sink).every((state) => state.status === TerminalViewStatus.REJECTED)).toBe(true);
    expect(terminalStates(unbound).map((state) => state.status)).toEqual([TerminalViewStatus.REJECTED]);
    expect(sent).toHaveLength(0);
    expect(hub.snapshot(SESSION)).toBeNull();

    hub.handleViewCommand("socket-a", viewCommand(VIEW_A, MAX_U64, { cols: 256, rows: 256 }));
    hub.handleViewCommand("socket-a", viewCommand(VIEW_B, 1n, { cols: 1, rows: 1 }));
    await settle();
    expect(hub.snapshot(SESSION)?.effective).toEqual({ cols: 1, rows: 1 });
  });

  test("enforces socket, session, and per-viewer tombstone capacities", async () => {
    const socketHarness = makeHarness();
    const socketSink = register(socketHarness.hub);
    for (let index = 0; index < 64; index += 1) {
      socketHarness.hub.handleViewCommand("socket-a", viewCommand(uuid(index + 1), 1n));
    }
    socketHarness.hub.handleViewCommand("socket-a", viewCommand(uuid(65), 1n));
    expect(socketHarness.hub.snapshot(SESSION)?.activeViews).toBe(64);
    expect(terminalStates(socketSink).at(-1)).toMatchObject({
      status: TerminalViewStatus.REJECTED,
      reason: "terminal socket view capacity exceeded",
    });

    const sessionHarness = makeHarness();
    for (let socketIndex = 0; socketIndex < 4; socketIndex += 1) {
      const socketId = `session-socket-${socketIndex}`;
      register(sessionHarness.hub, socketId, `viewer-${socketIndex}`, `fingerprint-${socketIndex}`);
      for (let viewIndex = 0; viewIndex < 64; viewIndex += 1) {
        sessionHarness.hub.handleViewCommand(
          socketId,
          viewCommand(uuid(socketIndex * 64 + viewIndex + 1), 1n),
        );
      }
    }
    const overflow = register(sessionHarness.hub, "session-overflow", "viewer-overflow", "fingerprint-overflow");
    sessionHarness.hub.handleViewCommand("session-overflow", viewCommand(uuid(300), 1n));
    expect(sessionHarness.hub.snapshot(SESSION)?.activeViews).toBe(256);
    expect(terminalStates(overflow).at(-1)).toMatchObject({
      status: TerminalViewStatus.REJECTED,
      reason: "terminal session view capacity exceeded",
    });

    const tombstoneHarness = makeHarness();
    const tombstoneSink = register(tombstoneHarness.hub);
    for (let index = 0; index < 129; index += 1) {
      tombstoneHarness.hub.handleViewCommand("socket-a", viewCommand(uuid(index + 1), 1n, {
        active: false,
        cols: 0,
        rows: 0,
      }));
    }
    tombstoneHarness.hub.handleViewCommand("socket-a", viewCommand(uuid(1), 1n));
    tombstoneHarness.hub.handleViewCommand("socket-a", viewCommand(uuid(2), 1n));
    await settle();
    expect(terminalStates(tombstoneSink).slice(-2).map((state) => state.status)).toEqual([
      TerminalViewStatus.ACCEPTED,
      TerminalViewStatus.REJECTED,
    ]);
  });

  test("purges membership and tombstones on fingerprint or session revocation", async () => {
    const fingerprintHarness = makeHarness();
    const viewerKey = "fingerprint-a:tab-a";
    register(fingerprintHarness.hub, "socket-a", viewerKey, "fingerprint-a");
    fingerprintHarness.hub.handleViewCommand("socket-a", viewCommand(VIEW_A, 5n, {
      active: false,
      cols: 0,
      rows: 0,
    }));
    fingerprintHarness.hub.removeFingerprint("fingerprint-a");
    const replacement = register(
      fingerprintHarness.hub,
      "socket-b",
      viewerKey,
      "fingerprint-a",
    );
    fingerprintHarness.hub.handleViewCommand("socket-b", viewCommand(VIEW_A, 5n));
    await settle();
    expect(statesFor(replacement, VIEW_A).at(-1)?.status).toBe(TerminalViewStatus.ACCEPTED);
    expect(fingerprintHarness.hub.snapshot(SESSION)).toMatchObject({ activeViews: 1 });

    const sessionHarness = makeHarness();
    const sessionSink = register(sessionHarness.hub);
    sessionHarness.hub.handleViewCommand("socket-a", viewCommand(VIEW_B, 9n, {
      active: false,
      cols: 0,
      rows: 0,
    }));
    sessionHarness.hub.closeSession(SESSION);
    sessionHarness.hub.handleViewCommand("socket-a", viewCommand(VIEW_B, 9n));
    await settle();
    expect(statesFor(sessionSink, VIEW_B).at(-1)?.status).toBe(TerminalViewStatus.ACCEPTED);
    expect(sessionHarness.hub.snapshot(SESSION)).toMatchObject({ activeViews: 1 });
  });
});
