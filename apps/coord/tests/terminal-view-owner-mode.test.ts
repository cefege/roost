// Owner mode: a worker that advertised terminal-view-owner-v1 owns membership,
// geometry and stream generations for its own sessions, so the coordinator
// relays browser view commands instead of minimizing them and turns the
// worker's answers back into screen expectations before any browser frame.
// The legacy case is asserted here too: it is what a released worker still
// needs from a new coordinator during an in-place upgrade.

import { afterEach, describe, expect, test } from "bun:test";
import { TerminalViewStatus } from "@roost/protocol/proto/sync_pb";
import {
  OWNER_FP,
  SOCKET,
  STREAM_A,
  STREAM_B,
  VIEWER_KEY,
  DEVICE_FP,
  makeOwnerHarness,
  ownerState,
  registerOwnerSocket,
  resetOwnerMode,
} from "./terminal-view-owner-harness.ts";
import {
  OTHER_SESSION,
  SESSION,
  VIEW_A,
  VIEW_B,
  WORKER,
  settle,
  terminalStates,
  viewCommand,
} from "./terminal-view-hub-harness.ts";
import { fullFrame } from "./terminal-screen-hub-harness.ts";

afterEach(resetOwnerMode);

describe("coordinator owner mode", () => {
  test("installs the screen expectation before the browser sees a new stream", () => {
    const { hub, calls } = makeOwnerHarness();
    const sink = registerOwnerSocket(hub, calls);

    hub.applyOwnerViewState(OWNER_FP, SOCKET, ownerState());

    expect(calls).toEqual([`expect_stream:${STREAM_A}`, "view_state"]);
    expect(terminalStates(sink)).toMatchObject([
      { viewId: VIEW_A, sessionId: SESSION, streamId: STREAM_A, effectiveCols: 80 },
    ]);
  });

  test("a rejection carries no stream and disturbs no expectation", () => {
    const { hub, calls } = makeOwnerHarness();
    registerOwnerSocket(hub, calls);

    hub.applyOwnerViewState(OWNER_FP, SOCKET, ownerState({
      streamId: "",
      status: TerminalViewStatus.REJECTED,
      effectiveCols: 0,
      effectiveRows: 0,
      reason: "terminal session is unavailable",
    }));

    expect(calls).toEqual(["view_state"]);
  });

  test("state from a worker that does not own the session never reaches the socket", () => {
    const { hub, calls } = makeOwnerHarness();
    registerOwnerSocket(hub, calls);

    hub.applyOwnerViewState("c".repeat(64), SOCKET, ownerState());

    expect(calls).toEqual([]);
  });

  // Nothing else calls screen.setWatching for an owner-mode session: the
  // shared registry, which owns every other call, never sees these commands.
  // Without the relay's own watch bookkeeping the remote pane paints nothing.
  test("cells fan out to an owner-mode socket and stop when its last view goes inactive", () => {
    const { hub, calls } = makeOwnerHarness();
    const sink = registerOwnerSocket(hub, calls);
    hub.applyOwnerViewState(OWNER_FP, SOCKET, ownerState({ effectiveCols: 8, effectiveRows: 2 }));

    hub.screen.publishFrame(SESSION, fullFrame({ streamId: STREAM_A }));

    expect(sink.snapshots).toMatchObject([{ sessionId: SESSION, streamId: STREAM_A }]);

    hub.applyOwnerViewState(OWNER_FP, SOCKET, ownerState({
      revision: 2n,
      active: false,
      streamId: "",
      effectiveCols: 0,
      effectiveRows: 0,
    }));

    expect(sink.drops).toEqual([SESSION]);
    hub.screen.publishFrame(SESSION, fullFrame({ streamId: STREAM_A, seq: 2n }));
    expect(sink.snapshots).toHaveLength(1);
  });

  // The coordinator's source-full repair lives in the stream controller, which
  // owner mode bypasses: it holds no session to resolve. Without a route back
  // to the owning worker the replica sits on a dead baseline and the remote
  // pane never repaints.
  test("a replica whose baseline is lost obtains a source full from the owning worker", () => {
    const { hub, calls, snapshotRequests } = makeOwnerHarness();
    const sink = registerOwnerSocket(hub, calls);
    const state = ownerState({ effectiveCols: 8, effectiveRows: 2 });
    hub.applyOwnerViewState(OWNER_FP, SOCKET, state);
    hub.screen.publishFrame(SESSION, fullFrame({ streamId: STREAM_A }));
    expect(sink.snapshots).toHaveLength(1);
    expect(snapshotRequests).toEqual([]);

    hub.screen.invalidate(SESSION, "worker upstream delta loss");

    expect(snapshotRequests).toEqual([
      { workerFp: OWNER_FP, sessionId: SESSION, streamId: STREAM_A },
    ]);

    // A view resuming on that same stream must not stack a second request.
    hub.applyOwnerViewState(OWNER_FP, SOCKET, ownerState({
      revision: 2n,
      effectiveCols: 8,
      effectiveRows: 2,
    }));
    expect(snapshotRequests).toHaveLength(1);
  });

  // The browser re-declares every live view on a lease heartbeat. Seeding on
  // those beats pushes a duplicate baseline to the pane every few seconds.
  test("a lease heartbeat on an attached view pushes no second baseline", () => {
    const { hub, calls } = makeOwnerHarness();
    const sink = registerOwnerSocket(hub, calls);
    hub.applyOwnerViewState(OWNER_FP, SOCKET, ownerState({ effectiveCols: 8, effectiveRows: 2 }));
    hub.screen.publishFrame(SESSION, fullFrame({ streamId: STREAM_A }));
    expect(sink.snapshots).toHaveLength(1);

    for (const revision of [2n, 3n]) {
      hub.applyOwnerViewState(OWNER_FP, SOCKET, ownerState({
        revision,
        effectiveCols: 8,
        effectiveRows: 2,
      }));
    }

    expect(sink.snapshots).toHaveLength(1);
  });

  test("a socket attaching to a stream the replica already holds is seeded from it", () => {
    const { hub, calls } = makeOwnerHarness();
    registerOwnerSocket(hub, calls);
    hub.applyOwnerViewState(OWNER_FP, SOCKET, ownerState({ effectiveCols: 8, effectiveRows: 2 }));
    hub.screen.publishFrame(SESSION, fullFrame({ streamId: STREAM_A }));
    const second = registerOwnerSocket(hub, calls, { socketId: "socket-b" });

    hub.applyOwnerViewState(OWNER_FP, "socket-b", ownerState({
      viewId: VIEW_B,
      effectiveCols: 8,
      effectiveRows: 2,
    }));

    expect(second.snapshots).toMatchObject([{ sessionId: SESSION, streamId: STREAM_A }]);
  });

  // A new stream's baseline comes from the worker's own stream install; asking
  // again would put two baselines on the single shared coord sink.
  test("a new stream id asks for no source full", () => {
    const { hub, calls, snapshotRequests } = makeOwnerHarness();
    registerOwnerSocket(hub, calls);

    hub.applyOwnerViewState(OWNER_FP, SOCKET, ownerState({ effectiveCols: 8, effectiveRows: 2 }));
    hub.applyOwnerViewState(OWNER_FP, SOCKET, ownerState({
      revision: 2n,
      streamId: STREAM_B,
      effectiveCols: 8,
      effectiveRows: 2,
    }));

    expect(snapshotRequests).toEqual([]);
  });

  test("an owner-mode session gets no coordinator stream desire and no controller state", async () => {
    const { hub, streamStates, relayed } = makeOwnerHarness();
    registerOwnerSocket(hub, []);

    hub.handleViewCommand(SOCKET, viewCommand(VIEW_A, 1n, { cols: 90, rows: 30 }));
    await settle();

    expect(streamStates).toEqual([]);
    expect(hub.snapshot(SESSION)).toBeNull();
    expect(hub.viewerInputs(SESSION)).toEqual([]);
    expect(relayed).toHaveLength(1);
  });

  test("an authorized view command relays with the authenticated tuple", () => {
    const { hub, relayed } = makeOwnerHarness();
    registerOwnerSocket(hub, []);
    const command = viewCommand(VIEW_A, 2n, { cols: 100, rows: 40 });

    hub.handleViewCommand(SOCKET, command);

    expect(relayed).toHaveLength(1);
    expect(relayed[0]).toMatchObject({
      workerFp: OWNER_FP,
      identity: { socketId: SOCKET, viewerKey: VIEWER_KEY, deviceFingerprint: DEVICE_FP },
      command: { case: "view" },
    });
    expect(relayed[0]!.command.value).toBe(command);
  });

  test("a command for a session outside the socket scope is refused, never relayed", () => {
    const { hub, relayed, calls } = makeOwnerHarness();
    const sink = registerOwnerSocket(hub, calls, { allowsSession: () => false });

    hub.handleViewCommand(SOCKET, viewCommand(VIEW_A, 7n));

    expect(relayed).toEqual([]);
    expect(terminalStates(sink)).toMatchObject([
      {
        viewId: VIEW_A,
        revision: 7n,
        status: TerminalViewStatus.REJECTED,
        streamId: "",
        reason: "terminal session is unavailable",
      },
    ]);
  });

  test("a socket with no viewer key is refused, never relayed", () => {
    const { hub, relayed, calls } = makeOwnerHarness();
    const sink = registerOwnerSocket(hub, calls, { viewerKey: null });

    hub.handleViewCommand(SOCKET, viewCommand(VIEW_A, 1n));

    expect(relayed).toEqual([]);
    expect(terminalStates(sink)).toMatchObject([
      {
        status: TerminalViewStatus.REJECTED,
        reason: "terminal views require a tab-bound Sync socket",
      },
    ]);
  });

  test("a revoked device stops reaching the owner before its socket closes", () => {
    const { hub, relayed, socketClosed } = makeOwnerHarness();
    registerOwnerSocket(hub, []);
    hub.handleViewCommand(SOCKET, viewCommand(VIEW_A, 1n));
    expect(relayed).toHaveLength(1);

    hub.removeFingerprint(DEVICE_FP);
    hub.handleViewCommand(SOCKET, viewCommand(VIEW_A, 2n));

    expect(relayed).toHaveLength(1);
    expect(socketClosed).toEqual([{ workerFp: OWNER_FP, socketId: SOCKET }]);
  });

  // A synthesized refusal has to echo the command's exact viewId and revision:
  // the browser store drops any view-state frame that does not resolve to a
  // live view at the revision it is waiting on, which would make this path
  // silently dead.
  test("an unreachable owner answers unavailable instead of admitting local membership", async () => {
    const { hub, streamStates, calls } = makeOwnerHarness({ relayAdmitted: false });
    const sink = registerOwnerSocket(hub, calls);

    hub.handleViewCommand(SOCKET, viewCommand(VIEW_A, 9n));
    await settle();

    expect(streamStates).toEqual([]);
    expect(terminalStates(sink)).toMatchObject([
      {
        viewId: VIEW_A,
        revision: 9n,
        active: true,
        status: TerminalViewStatus.UNAVAILABLE,
        reason: "terminal worker is unavailable",
      },
    ]);
  });

  test("closing a browser socket tells every owner it relayed to", () => {
    const { hub, socketClosed } = makeOwnerHarness();
    registerOwnerSocket(hub, []);
    registerOwnerSocket(hub, [], { socketId: "socket-b" });
    hub.handleViewCommand(SOCKET, viewCommand(VIEW_A, 1n));

    hub.closeSocket("socket-b");
    expect(socketClosed).toEqual([]);

    hub.closeSocket(SOCKET);
    expect(socketClosed).toEqual([{ workerFp: OWNER_FP, socketId: SOCKET }]);
  });
});

describe("legacy mode", () => {
  test("a legacy worker still drives the coordinator stream controller", async () => {
    const { hub, streamStates, relayed } = makeOwnerHarness({ owner: null });
    const sink = registerOwnerSocket(hub, []);

    hub.handleViewCommand(SOCKET, viewCommand(VIEW_A, 1n, { cols: 90, rows: 30 }));
    await settle();

    expect(relayed).toEqual([]);
    expect(streamStates).toMatchObject([
      { workerFp: WORKER, sessionId: SESSION, cols: 90, rows: 30 },
    ]);
    expect(terminalStates(sink).at(-1)).toMatchObject({
      viewId: VIEW_A,
      status: TerminalViewStatus.ACCEPTED,
      effectiveCols: 90,
      effectiveRows: 30,
    });
    expect(hub.snapshot(SESSION)).toMatchObject({
      activeViews: 1,
      effective: { cols: 90, rows: 30 },
    });
  });

  test("a legacy socket close notifies no owner", () => {
    const { hub, socketClosed } = makeOwnerHarness({ owner: null });
    registerOwnerSocket(hub, []);
    hub.handleViewCommand(SOCKET, viewCommand(VIEW_A, 1n));

    hub.closeSocket(SOCKET);

    expect(socketClosed).toEqual([]);
  });

  test("a legacy worker's route reconcile still redrives its stream", async () => {
    const { hub, streamStates } = makeOwnerHarness({ owner: null });
    registerOwnerSocket(hub, []);
    hub.handleViewCommand(SOCKET, viewCommand(VIEW_A, 1n, { cols: 90, rows: 30 }));
    await settle();
    expect(streamStates).toHaveLength(1);

    hub.routeReconciled(WORKER, [SESSION]);
    await settle();

    expect(streamStates).toHaveLength(2);
    expect(streamStates[1]!.streamId).not.toBe(streamStates[0]!.streamId);
  });

  test("a worker that upgrades in place takes over the coordinator's session", async () => {
    const { hub, ownerRef, streamStates, relayed } = makeOwnerHarness({ owner: null });
    registerOwnerSocket(hub, []);
    hub.handleViewCommand(SOCKET, viewCommand(VIEW_A, 1n, { cols: 90, rows: 30 }));
    await settle();
    expect(hub.snapshot(SESSION)).not.toBeNull();

    ownerRef.value = OWNER_FP;
    hub.routeReconciled(OWNER_FP, [SESSION]);
    await settle();

    expect(hub.snapshot(SESSION)).toBeNull();
    expect(hub.viewerInputs(SESSION)).toEqual([]);
    expect(streamStates).toHaveLength(1);

    hub.handleViewCommand(SOCKET, viewCommand(VIEW_A, 2n, { cols: 90, rows: 30 }));
    expect(relayed).toHaveLength(1);
    expect(streamStates).toHaveLength(1);
  });

  test("a mixed reconcile batch releases only the owned session and reconciles the rest", async () => {
    const { hub, ownerRef, streamStates } = makeOwnerHarness({ owner: null });
    registerOwnerSocket(hub, []);
    hub.handleViewCommand(SOCKET, viewCommand(VIEW_A, 1n, { cols: 90, rows: 30 }));
    hub.handleViewCommand(SOCKET, viewCommand(VIEW_B, 1n, { sessionId: OTHER_SESSION }));
    await settle();
    expect(streamStates).toHaveLength(2);

    ownerRef.bySession.set(SESSION, WORKER);
    hub.routeReconciled(WORKER, [SESSION, OTHER_SESSION]);
    await settle();

    expect(hub.snapshot(SESSION)).toBeNull();
    expect(streamStates.filter((state) => state.sessionId === SESSION)).toHaveLength(1);
    expect(streamStates.filter((state) => state.sessionId === OTHER_SESSION)).toHaveLength(2);
  });

  test("an owner-mode reconnect leaves a session the coordinator never minimized alone", () => {
    const { hub } = makeOwnerHarness();
    const sink = registerOwnerSocket(hub, []);
    hub.applyOwnerViewState(OWNER_FP, SOCKET, ownerState());

    hub.routeReconciled(OWNER_FP, [SESSION]);

    expect(sink.drops).toEqual([]);
  });
});
