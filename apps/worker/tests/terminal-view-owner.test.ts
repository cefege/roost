// Proves the worker is the authority for its own sessions' terminal views:
// geometry is the minimum over live viewers, a parked viewer stops constraining
// only when its grace lapses, losing every viewer HOLDS the last geometry, a
// view decision always precedes its stream's first cell, and a coordinator
// reconnect drops only coordinator-relayed sockets. The real registry, session
// manager, cell sinks and keeper socket run; only the clock is injected.

import { create } from "@bufbuild/protobuf";
import { afterEach, describe, expect, test } from "bun:test";
import { randomUUID } from "node:crypto";
import type { PbCellGridChunk, PbCellGridFrame } from "@roost/protocol/proto/cell_pb";
import {
  TerminalViewCommandSchema,
  TerminalViewStatus,
  type TerminalViewStateFrame,
} from "@roost/protocol/proto/sync_pb";
import { DTerminalViewRelaySchema } from "@roost/protocol/proto/worker_transport_pb";
import {
  TERMINAL_VIEW_LEASE_MS,
  TERMINAL_VIEW_PARK_GRACE_MS,
} from "@roost/protocol/viewport";
import { TerminalViewOwner } from "../src/terminal-view-owner.ts";
import type { LocalViewTransport } from "../src/terminal-view-owner-screen.ts";
import type { TerminalViewProjectionFrame } from "../src/transport/coord-link-types.ts";
import { installAutoKeeper, type AutoKeeperOptions } from "./keeper-fake-pool.ts";
import { getMultiplexedPool } from "../src/keeper/multiplexed-client.ts";
import {
  CHANNEL_ID,
  cleanupStreamHarnesses,
  flushLeadingCellEmit,
  makeHarness,
  SESSION_ID,
  TEST_COLS,
  TEST_ROWS,
  trackKeeper,
  type StreamHarness,
} from "./terminal-stream-state-harness.ts";

const DEVICE = "a".repeat(64);
const REMOTE_DEVICE = "b".repeat(64);

/** Everything one socket observed, in the order the owner produced it, so
 *  "state before cells" is checked as an order and not as two counters. */
interface RecordedSocket {
  socketId: string;
  transport: LocalViewTransport;
  states: TerminalViewStateFrame[];
  order: string[];
  overflows: number;
  expiries: number;
}

interface OwnerFixture {
  harness: StreamHarness;
  owner: TerminalViewOwner;
  projections: TerminalViewProjectionFrame[];
  relayed: { socketId: string; frame: TerminalViewStateFrame }[];
  advance(ms: number): void;
  sweep(): void;
}

const owners: TerminalViewOwner[] = [];

afterEach(() => {
  for (const owner of owners.splice(0)) owner.dispose();
  cleanupStreamHarnesses();
});

async function makeOwner(keeperOptions: AutoKeeperOptions = {}): Promise<OwnerFixture> {
  trackKeeper(installAutoKeeper({ cols: TEST_COLS, rows: TEST_ROWS }, keeperOptions));
  const harness = await makeHarness();
  let clock = 1_000;
  const projections: TerminalViewProjectionFrame[] = [];
  const relayed: { socketId: string; frame: TerminalViewStateFrame }[] = [];
  const owner = new TerminalViewOwner({
    sessions: () => harness.manager,
    sendViewState: (socketId, frame) => { relayed.push({ socketId, frame }); },
    sendProjection: (projection) => { projections.push(projection); },
    now: () => clock,
  });
  owners.push(owner);
  return {
    harness,
    owner,
    projections,
    relayed,
    advance: (ms) => { clock += ms; },
    // The production sweep is an interval; driving it explicitly keeps the
    // lease and park-grace assertions deterministic.
    sweep: () => { owner._sweep(); },
  };
}

/** Every stream generation the owner told this socket about, in order. One
 *  entry per `desire`, which is what bounds a self-healing retry. */
function acceptedStreamIds(socket: RecordedSocket): string[] {
  return socket.states
    .filter((frame) => frame.status === TerminalViewStatus.ACCEPTED)
    .map((frame) => frame.streamId);
}

/** Keeper history for a channel that has produced nothing: the re-proof
 *  rebuilds an empty grid at the keeper's own geometry. The fake keeper socket
 *  never answers history frames, so the read is stubbed at the pool. */
function stubEmptyKeeperHistory(): () => void {
  const pool = getMultiplexedPool();
  const prior = pool.getHistoryRecords.bind(pool);
  pool.getHistoryRecords = async () => ({
    headSeq: 0,
    baseCols: TEST_COLS,
    baseRows: TEST_ROWS,
    records: [],
  });
  return () => { pool.getHistoryRecords = prior; };
}

function stubRefusedKeeperHistory(): () => void {
  const pool = getMultiplexedPool();
  const prior = pool.getHistoryRecords.bind(pool);
  pool.getHistoryRecords = async () => {
    throw new Error("ordered history is unavailable");
  };
  return () => { pool.getHistoryRecords = prior; };
}

function localSocket(fixture: OwnerFixture, tabId: string): RecordedSocket {
  const socketId = randomUUID();
  const recorded: RecordedSocket = {
    socketId,
    states: [],
    order: [],
    overflows: 0,
    expiries: 0,
    transport: {
      kind: "local",
      sendViewState: (frame) => {
        recorded.states.push(frame);
        recorded.order.push(`state:${frame.streamId}:${frame.status}`);
      },
      sendCellFrame: (frame: PbCellGridFrame) => {
        recorded.order.push(`cell:${frame.streamId}`);
        return "sent";
      },
      sendCellChunk: (chunk: PbCellGridChunk) => {
        recorded.order.push(`chunk:${chunk.snapshotId}`);
        return "sent";
      },
      onOverflow: () => { recorded.overflows += 1; },
      onViewExpired: () => { recorded.expiries += 1; },
    },
  };
  fixture.owner.registerLocalSocket({
    socketId,
    deviceFingerprint: DEVICE,
    tabId,
    allowsSession: (sessionId) => sessionId === String(SESSION_ID),
    transport: recorded.transport,
  });
  return recorded;
}

function viewCommand(viewId: string, cols: number, rows: number, revision = 1n) {
  return create(TerminalViewCommandSchema, {
    viewId,
    sessionId: String(SESSION_ID),
    cols,
    rows,
    revision,
    active: true,
  });
}

function relayView(
  fixture: OwnerFixture,
  socketId: string,
  viewId: string,
  cols: number,
  rows: number,
): void {
  fixture.owner.handleRelay(create(DTerminalViewRelaySchema, {
    socketId,
    viewerKey: `${REMOTE_DEVICE}:remote-tab`,
    deviceFingerprint: REMOTE_DEVICE,
    budgetMs: 8_000,
    command: { case: "view", value: viewCommand(viewId, cols, rows) },
  }));
}

function geometry(harness: StreamHarness): { streamId: string; cols: number; rows: number } {
  const state = harness.manager.terminalStreams.get(CHANNEL_ID);
  if (!state) throw new Error("no terminal stream was installed");
  return { streamId: state.streamId, cols: state.cols, rows: state.rows };
}

/** The owner's stream work rides the terminal-control lane, and the stream
 *  state exposes that operation. Settling means "the stream stopped being
 *  replaced": a retry desire installs a fresh stream with its own operation. */
async function settle(harness: StreamHarness): Promise<void> {
  let pending = harness.manager.terminalStreams.get(CHANNEL_ID)?.operation;
  while (pending) {
    await pending;
    await flushLeadingCellEmit();
    const next = harness.manager.terminalStreams.get(CHANNEL_ID)?.operation;
    if (next === pending) return;
    pending = next;
  }
}

describe("worker-owned terminal views", () => {
  test("effective geometry is the minimum over live viewers", async () => {
    const fixture = await makeOwner();
    const wide = localSocket(fixture, "tab-wide");
    const narrow = localSocket(fixture, "tab-narrow");

    fixture.owner.handleViewCommand(wide.socketId, viewCommand(randomUUID(), 100, 30));
    await settle(fixture.harness);
    expect(geometry(fixture.harness)).toMatchObject({ cols: 100, rows: 30 });

    fixture.owner.handleViewCommand(narrow.socketId, viewCommand(randomUUID(), 80, 24));
    await settle(fixture.harness);

    expect(geometry(fixture.harness)).toMatchObject({ cols: 80, rows: 24 });
    const last = wide.states.at(-1)!;
    expect(last).toMatchObject({
      status: TerminalViewStatus.ACCEPTED,
      effectiveCols: 80,
      effectiveRows: 24,
    });
  });

  test("a parked viewer stops constraining only once its grace lapses", async () => {
    const fixture = await makeOwner();
    const wide = localSocket(fixture, "tab-wide");
    const narrow = localSocket(fixture, "tab-narrow");
    fixture.owner.handleViewCommand(wide.socketId, viewCommand(randomUUID(), 100, 30));
    fixture.owner.handleViewCommand(narrow.socketId, viewCommand(randomUUID(), 80, 24));
    await settle(fixture.harness);
    const constrained = geometry(fixture.harness);

    fixture.owner.closeSocket(narrow.socketId);
    fixture.sweep();
    await settle(fixture.harness);
    // Park absorbs reconnect wobble: inside the grace the PTY must not resize.
    expect(geometry(fixture.harness)).toEqual(constrained);

    fixture.advance(TERMINAL_VIEW_PARK_GRACE_MS + 1);
    fixture.sweep();
    await settle(fixture.harness);

    expect(geometry(fixture.harness)).toMatchObject({ cols: 100, rows: 30 });
    expect(geometry(fixture.harness).streamId).not.toBe(constrained.streamId);
  });

  test("losing every live viewer holds the last geometry and mints no stream", async () => {
    const fixture = await makeOwner();
    const wide = localSocket(fixture, "tab-wide");
    const narrow = localSocket(fixture, "tab-narrow");
    fixture.owner.handleViewCommand(wide.socketId, viewCommand(randomUUID(), 100, 30));
    fixture.owner.handleViewCommand(narrow.socketId, viewCommand(randomUUID(), 80, 24));
    await settle(fixture.harness);
    const held = geometry(fixture.harness);

    fixture.owner.closeSocket(wide.socketId);
    fixture.owner.closeSocket(narrow.socketId);
    fixture.advance(TERMINAL_VIEW_PARK_GRACE_MS + 1);
    fixture.sweep();
    await settle(fixture.harness);

    expect(geometry(fixture.harness)).toEqual(held);
  });

  test("a view decision reaches the socket before its stream's first cell", async () => {
    const fixture = await makeOwner();
    const pane = localSocket(fixture, "tab-pane");

    fixture.owner.handleViewCommand(pane.socketId, viewCommand(randomUUID(), 40, 12));
    await settle(fixture.harness);

    const streamId = geometry(fixture.harness).streamId;
    const stateAt = pane.order.findIndex((entry) => entry.startsWith(`state:${streamId}:`));
    const cellAt = pane.order.findIndex((entry) => entry === `cell:${streamId}`);
    expect(stateAt).toBeGreaterThanOrEqual(0);
    expect(cellAt).toBeGreaterThan(stateAt);
    expect(pane.states[stateAt]).toMatchObject({
      status: TerminalViewStatus.ACCEPTED,
      streamId,
      effectiveCols: 40,
      effectiveRows: 12,
    });
  });

  test("a coordinator reconnect drops only its own sockets", async () => {
    const fixture = await makeOwner();
    const pane = localSocket(fixture, "tab-pane");
    const paneViewId = randomUUID();
    const remoteSocketId = randomUUID();
    fixture.owner.handleViewCommand(pane.socketId, viewCommand(paneViewId, 100, 30));
    relayView(fixture, remoteSocketId, randomUUID(), 80, 24);
    await settle(fixture.harness);
    const shared = geometry(fixture.harness);
    expect(shared).toMatchObject({ cols: 80, rows: 24 });
    expect(fixture.relayed.at(-1)).toMatchObject({ socketId: remoteSocketId });

    fixture.owner.dropCoordinatorSockets();
    await settle(fixture.harness);
    // Nothing about the live stream may move on a coordinator bounce.
    expect(geometry(fixture.harness)).toEqual(shared);
    expect(fixture.harness.manager.cellSinks.has(`local:${pane.socketId}`)).toBe(true);

    fixture.advance(TERMINAL_VIEW_PARK_GRACE_MS + 1);
    fixture.sweep();
    await settle(fixture.harness);
    expect(geometry(fixture.harness)).toMatchObject({ cols: 100, rows: 30 });
    expect(fixture.projections.at(-1)!.viewers).toEqual([
      expect.objectContaining({ fingerprint: DEVICE, parked: false, constrains: true }),
      expect.objectContaining({ fingerprint: REMOTE_DEVICE, parked: true, constrains: false }),
    ]);

    // The local view's lease is untouched, so renewing it at the same revision
    // keeps it live past the tick that reaps the coordinator's parked record.
    fixture.advance(TERMINAL_VIEW_LEASE_MS - TERMINAL_VIEW_PARK_GRACE_MS);
    fixture.owner.handleViewCommand(pane.socketId, viewCommand(paneViewId, 100, 30));
    fixture.sweep();
    await settle(fixture.harness);
    const projection = fixture.projections.at(-1)!;
    expect(projection.viewers.map((viewer) => viewer.fingerprint)).toEqual([DEVICE]);
    expect(projection.viewers[0]).toMatchObject({ cols: 100, rows: 30, constrains: true });
    expect(projection).toMatchObject({ effectiveCols: 100, effectiveRows: 30 });
    expect(pane.expiries).toBe(0);
  });

  test("a trapped core re-proves itself on the desire the trap triggers", async () => {
    // The first sequenced resize comes back acked at a size nobody asked for:
    // the boundary can never be proven, so the apply answers core_failed.
    const fixture = await makeOwner({ trapResizeSeqs: [1] });
    const pane = localSocket(fixture, "tab-pane");

    const restoreHistory = stubEmptyKeeperHistory();
    try {
      fixture.owner.handleViewCommand(pane.socketId, viewCommand(randomUUID(), 40, 12));
      await settle(fixture.harness);
    } finally {
      restoreHistory();
    }

    // The trap drives exactly ONE further desire — the re-proof attempt. A
    // third generation here would mean the repair loops on its own verdict.
    const desired = acceptedStreamIds(pane);
    expect(desired).toHaveLength(2);
    expect(desired[1]).not.toBe(desired[0]);
    expect(pane.states.at(-1)).toMatchObject({
      status: TerminalViewStatus.ACCEPTED,
      streamId: desired[1],
      effectiveCols: 40,
      effectiveRows: 12,
    });
    expect(fixture.harness.manager.terminalStreams.get(CHANNEL_ID)!.coreValid).toBe(true);
    expect(geometry(fixture.harness)).toMatchObject({
      streamId: desired[1],
      cols: 40,
      rows: 12,
    });
    expect(pane.order).toContain(`cell:${desired[1]}`);
  });

  test("a trap the keeper cannot re-prove stays fail-closed and desires nothing more", async () => {
    const fixture = await makeOwner({ trapResizeSeqs: [1] });
    const pane = localSocket(fixture, "tab-pane");

    const restoreHistory = stubRefusedKeeperHistory();
    try {
      fixture.owner.handleViewCommand(pane.socketId, viewCommand(randomUUID(), 40, 12));
      await settle(fixture.harness);
    } finally {
      restoreHistory();
    }

    expect(acceptedStreamIds(pane)).toHaveLength(2);
    expect(pane.states.at(-1)).toMatchObject({ status: TerminalViewStatus.UNAVAILABLE });
    expect(fixture.harness.manager.terminalStreams.get(CHANNEL_ID)!.coreValid).toBe(false);
  });
});
