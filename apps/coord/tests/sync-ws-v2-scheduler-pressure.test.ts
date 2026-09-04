// This file owns scheduler overflow and stalled-progress recovery coverage.
// Bun's coord test suite runs these cases through the shared scheduler harness.
// It depends on protobuf terminal state frames, queue limits, and ACK deadlines.

import { expect, test } from "bun:test";
import { create } from "@bufbuild/protobuf";
import {
  FirehoseFrameSchema,
  SyncDomain,
  TerminalViewStateFrameSchema,
  TerminalViewStatus,
  type FirehoseFrame,
} from "@roost/shared/proto/sync_pb";
import {
  V2_AGGREGATE_MAX_QUEUED_BYTES,
  V2_AGGREGATE_MAX_QUEUED_FRAMES,
  V2_DOMAIN_MAX_QUEUED_FRAMES,
} from "../src/connect/sync-ws-v2-state.ts";
import { APPLICATION_ACK_TIMEOUT_MS } from "../src/connect/sync-ws-v1-delivery.ts";
import {
  SESSION_A,
  TARGET_SESSION,
  decodedFrames,
  fillApplicationAckWindow,
  flushMicrotasks,
  makeCell,
  makeHarness,
  makeState,
} from "./sync-ws-v2-scheduler-harness.ts";

// Pending view-state overflow is terminal-domain loss, not a superseding queue:
// reset the domain so every view replays instead of silently shedding state.
const pendingState = (
  sessionId: string,
  streamId: string,
  revision: bigint,
): FirehoseFrame =>
  create(FirehoseFrameSchema, {
    frame: {
      case: "terminalViewState",
      value: create(TerminalViewStateFrameSchema, {
        viewId: "55555555-5555-4555-8555-555555555555",
        sessionId,
        revision,
        active: true,
        streamId,
        status: TerminalViewStatus.ACCEPTED,
        effectiveCols: 80,
        effectiveRows: 24,
      }),
    },
  });
const paddedState = (
  sessionId: string,
  streamId: string,
  revision: bigint,
  paddingBytes: number,
): FirehoseFrame => {
  const frame = pendingState(sessionId, streamId, revision);
  if (frame.frame.case !== "terminalViewState") throw new TypeError("expected terminal state");
  frame.frame.value.streamId += "s".repeat(paddingBytes);
  return frame;
};

const paddedCell = (
  sessionId: string,
  seq: number,
  full: boolean,
  paddingBytes: number,
): FirehoseFrame => {
  const frame = makeCell(sessionId, seq, full);
  if (frame.frame.case !== "cellGrid") throw new TypeError("expected cell grid");
  frame.frame.value.gridEpoch = "g".repeat(paddingBytes);
  return frame;
};


test("a blocked terminal pending-state overflow resets the terminal domain", () => {
  const harness = makeHarness("scheduler-test:pending-states-overflow", true);
  const streamId = "stream-pending-overflow";
  const pushed = V2_DOMAIN_MAX_QUEUED_FRAMES + 1;
  const generation = harness.terminal.generation;
  harness.scheduler.beginTerminalStream(harness.ws, SESSION_A, streamId);
  harness.terminal.subscribed = false;
  for (let seq = 1; seq <= pushed; seq += 1) {
    harness.scheduler.enqueueTerminalState(
      harness.ws,
      pendingState(SESSION_A, streamId, BigInt(seq)),
      SESSION_A,
    );
  }

  expect(harness.terminal.generation).not.toBe(generation);
  expect(harness.terminal.ready).toBe(false);
  expect(harness.terminal.queue).toEqual([]);
  expect(harness.socket.data.v2!.terminalSessions.size).toBe(0);
  expect(decodedFrames(harness.socket).at(-1)?.frame).toMatchObject({
    case: "domainReset",
    value: {
      domain: SyncDomain.TERMINAL,
      reason: "terminal_pending_states_overflow",
    },
  });
  expect(harness.socket.closes).toEqual([]);
});

test("unready terminal work resets after one ACK-budget progress deadline", () => {
  const harness = makeHarness("scheduler-test:queued-progress", false);
  const streamId = "stream-queued-progress";
  const generation = harness.terminal.generation;
  harness.scheduler.beginTerminalStream(harness.ws, TARGET_SESSION, streamId);

  expect(harness.scheduler.enqueueTerminalDelta(
    harness.ws,
    TARGET_SESSION,
    streamId,
    makeCell(TARGET_SESSION, 1, false),
  )).toBe(true);
  const timer = harness.socket.data.v2!.terminalProgressTimer;
  expect(timer).not.toBeNull();
  expect(harness.clock.pendingTimerCount(APPLICATION_ACK_TIMEOUT_MS)).toBe(1);

  harness.clock.advance(APPLICATION_ACK_TIMEOUT_MS);
  expect(harness.clock.runTimer(timer!)).toBe(true);
  expect(harness.terminal.generation).not.toBe(generation);
  expect(harness.terminal.queue).toEqual([]);
  expect(decodedFrames(harness.socket).at(-1)?.frame).toMatchObject({
    case: "domainReset",
    value: { reason: "queued_progress_timeout" },
  });
});

test("a sent session announcement leaves terminal progress to the ACK timer alone", async () => {
  const harness = makeHarness("scheduler-test:announcement-ack-owner", true);
  const streamId = "stream-announcement-ack-owner";
  harness.scheduler.beginTerminalStream(harness.ws, TARGET_SESSION, streamId);

  expect(harness.scheduler.enqueueV2Frame(
    harness.ws,
    makeState(TARGET_SESSION, streamId),
    {
      domain: SyncDomain.TERMINAL,
      lane: "session",
      announces: [TARGET_SESSION],
    },
  )).toBe(true);
  expect(harness.scheduler.enqueueTerminalDelta(
    harness.ws,
    TARGET_SESSION,
    streamId,
    makeCell(TARGET_SESSION, 1, false),
  )).toBe(true);
  await flushMicrotasks();

  expect(harness.socket.data.v2!.pendingSessionAnnouncements.has(TARGET_SESSION)).toBe(true);
  expect(harness.socket.data.v2!.terminalProgressTimer).toBeNull();
  expect(harness.socket.data.deliveryTimer).not.toBeNull();
  expect(harness.terminal.queue).toHaveLength(1);
});

test("terminal domain queue overflow resets instead of dropping the next cell", () => {
  const harness = makeHarness("scheduler-test:domain-overflow", false);
  const generation = harness.terminal.generation;
  for (let seq = 1; seq <= V2_DOMAIN_MAX_QUEUED_FRAMES; seq += 1) {
    expect(harness.scheduler.enqueueV2Frame(
      harness.ws,
      makeCell(TARGET_SESSION, seq, false),
      { domain: SyncDomain.TERMINAL, lane: "cell", sessionId: TARGET_SESSION },
    )).toBe(true);
  }
  expect(harness.scheduler.enqueueV2Frame(
    harness.ws,
    makeCell(TARGET_SESSION, V2_DOMAIN_MAX_QUEUED_FRAMES + 1, false),
    { domain: SyncDomain.TERMINAL, lane: "cell", sessionId: TARGET_SESSION },
  )).toBe(false);

  expect(harness.terminal.generation).not.toBe(generation);
  expect(harness.terminal.queue).toEqual([]);
  expect(decodedFrames(harness.socket).at(-1)?.frame).toMatchObject({
    case: "domainReset",
    value: { reason: "domain_overflow" },
  });
});

test("aggregate queue overflow resets the terminal domain independently", () => {
  const harness = makeHarness("scheduler-test:aggregate-overflow", false);
  const v2 = harness.socket.data.v2!;
  const workers = v2.domains.get(SyncDomain.WORKERS)!;
  const workspaces = v2.domains.get(SyncDomain.WORKSPACES)!;
  const filler = makeState(TARGET_SESSION, "aggregate-filler");
  for (let index = 0; index < V2_DOMAIN_MAX_QUEUED_FRAMES; index += 1) {
    expect(harness.scheduler.enqueueV2Frame(
      harness.ws,
      filler,
      { domain: SyncDomain.WORKERS, lane: "retained" },
    )).toBe(true);
    expect(harness.scheduler.enqueueV2Frame(
      harness.ws,
      filler,
      { domain: SyncDomain.WORKSPACES, lane: "session" },
    )).toBe(true);
  }
  expect(workers.queue).toHaveLength(V2_DOMAIN_MAX_QUEUED_FRAMES);
  expect(workspaces.queue).toHaveLength(V2_DOMAIN_MAX_QUEUED_FRAMES);
  expect(v2.queuedFrames).toBe(V2_AGGREGATE_MAX_QUEUED_FRAMES);

  expect(harness.scheduler.enqueueV2Frame(
    harness.ws,
    makeCell(TARGET_SESSION, 1, false),
    { domain: SyncDomain.TERMINAL, lane: "cell", sessionId: TARGET_SESSION },
  )).toBe(false);
  expect(decodedFrames(harness.socket).at(-1)?.frame).toMatchObject({
    case: "domainReset",
    value: { reason: "aggregate_overflow" },
  });
  expect(workers.queue).toHaveLength(V2_DOMAIN_MAX_QUEUED_FRAMES);
  expect(workspaces.queue).toHaveLength(V2_DOMAIN_MAX_QUEUED_FRAMES);
});

test("terminal delta-tail overflow resets instead of returning a silent false", () => {
  const harness = makeHarness("scheduler-test:delta-tail-overflow", true);
  const streamId = "stream-delta-tail-overflow";
  harness.scheduler.beginTerminalStream(harness.ws, TARGET_SESSION, streamId);
  harness.socket.data.v2!.announcedSessions.add(TARGET_SESSION);
  fillApplicationAckWindow(harness);
  harness.scheduler.replaceTerminalSnapshot(
    harness.ws,
    TARGET_SESSION,
    streamId,
    [makeCell(TARGET_SESSION, 1, true)],
  );
  for (let seq = 2; seq <= V2_DOMAIN_MAX_QUEUED_FRAMES + 1; seq += 1) {
    expect(harness.scheduler.enqueueTerminalDelta(
      harness.ws,
      TARGET_SESSION,
      streamId,
      makeCell(TARGET_SESSION, seq, false),
    )).toBe(true);
  }
  expect(harness.scheduler.enqueueTerminalDelta(
    harness.ws,
    TARGET_SESSION,
    streamId,
    makeCell(TARGET_SESSION, V2_DOMAIN_MAX_QUEUED_FRAMES + 2, false),
  )).toBe(false);
  expect(harness.socket.data.v2!.terminalSessions.size).toBe(0);
  expect(decodedFrames(harness.socket).at(-1)?.frame).toMatchObject({
    case: "domainReset",
    value: { reason: "terminal_delta_tail_overflow" },
  });
});

test("aggregate frame budget includes domain queues, snapshot cursors, delta tails, and pending states", () => {
  const harness = makeHarness("scheduler-test:aggregate-terminal-frames", false);
  const v2 = harness.socket.data.v2!;
  const workers = v2.domains.get(SyncDomain.WORKERS)!;
  const workspaces = v2.domains.get(SyncDomain.WORKSPACES)!;
  const terminalGeneration = harness.terminal.generation;
  for (let index = 0; index < 300; index += 1) {
    expect(harness.scheduler.enqueueV2Frame(
      harness.ws,
      makeCell(SESSION_A, index + 1, false),
      { domain: SyncDomain.WORKERS, lane: "retained" },
    )).toBe(true);
    expect(harness.scheduler.enqueueV2Frame(
      harness.ws,
      makeCell(TARGET_SESSION, index + 1_001, false),
      { domain: SyncDomain.WORKSPACES, lane: "session" },
    )).toBe(true);
  }

  harness.terminal.subscribed = false;
  const snapshotStream = "aggregate-frame-snapshot";
  harness.scheduler.beginTerminalStream(harness.ws, SESSION_A, snapshotStream);
  harness.scheduler.replaceTerminalSnapshot(
    harness.ws,
    SESSION_A,
    snapshotStream,
    Array.from(
      { length: 150 },
      (_, index) => makeCell(SESSION_A, index + 2_001, index === 0),
    ),
  );
  for (let index = 0; index < 100; index += 1) {
    expect(harness.scheduler.enqueueTerminalDelta(
      harness.ws,
      SESSION_A,
      snapshotStream,
      makeCell(SESSION_A, index + 3_001, false),
    )).toBe(true);
  }
  const stateStream = "aggregate-frame-states";
  harness.scheduler.beginTerminalStream(harness.ws, TARGET_SESSION, stateStream);
  for (let index = 0; index < 174; index += 1) {
    harness.scheduler.enqueueTerminalState(
      harness.ws,
      pendingState(TARGET_SESSION, stateStream, BigInt(index + 1)),
      TARGET_SESSION,
    );
  }

  expect(v2.queuedFrames).toBe(V2_AGGREGATE_MAX_QUEUED_FRAMES);
  expect(harness.terminal.generation).toBe(terminalGeneration);
  expect(v2.terminalSessions.get(SESSION_A)?.cursor?.frames).toHaveLength(150);
  expect(v2.terminalSessions.get(SESSION_A)?.cursor?.deltaTail).toHaveLength(100);
  expect(v2.terminalSessions.get(TARGET_SESSION)?.pendingStates).toHaveLength(174);
  harness.scheduler.enqueueTerminalState(
    harness.ws,
    pendingState(TARGET_SESSION, stateStream, 175n),
    TARGET_SESSION,
  );

  expect(harness.terminal.generation).not.toBe(terminalGeneration);
  expect(v2.terminalSessions.size).toBe(0);
  expect(v2.queuedFrames).toBe(600);
  expect(workers.queue).toHaveLength(300);
  expect(workspaces.queue).toHaveLength(300);
  expect(workers.queue.map((item) => item.frame.frame.case)).toEqual(
    Array.from({ length: 300 }, () => "cellGrid"),
  );
  expect(workers.queue.map((item) => item.frame.frame.case === "cellGrid"
    ? item.frame.frame.value.seq
    : -1n)).toEqual(Array.from({ length: 300 }, (_, index) => BigInt(index + 1)));
  expect(workspaces.queue.map((item) => item.frame.frame.case === "cellGrid"
    ? item.frame.frame.value.seq
    : -1n)).toEqual(Array.from({ length: 300 }, (_, index) => BigInt(index + 1_001)));
  expect(decodedFrames(harness.socket).at(-1)?.frame).toMatchObject({
    case: "domainReset",
    value: { domain: SyncDomain.TERMINAL, reason: "aggregate_overflow" },
  });
  expect(harness.socket.closes).toEqual([]);
});

test("aggregate byte budget includes every terminal auxiliary owner", () => {
  const harness = makeHarness("scheduler-test:aggregate-terminal-bytes", false);
  const v2 = harness.socket.data.v2!;
  const workers = v2.domains.get(SyncDomain.WORKERS)!;
  const workspaces = v2.domains.get(SyncDomain.WORKSPACES)!;
  const mebibyte = 1024 * 1024;
  expect(harness.scheduler.enqueueV2Frame(
    harness.ws,
    paddedState(SESSION_A, "worker-bytes", 1n, 2 * mebibyte),
    { domain: SyncDomain.WORKERS, lane: "retained" },
  )).toBe(true);
  expect(harness.scheduler.enqueueV2Frame(
    harness.ws,
    paddedState(TARGET_SESSION, "workspace-bytes", 2n, 2 * mebibyte),
    { domain: SyncDomain.WORKSPACES, lane: "session" },
  )).toBe(true);

  harness.terminal.subscribed = false;
  const snapshotStream = "aggregate-byte-snapshot";
  harness.scheduler.beginTerminalStream(harness.ws, SESSION_A, snapshotStream);
  harness.scheduler.replaceTerminalSnapshot(
    harness.ws,
    SESSION_A,
    snapshotStream,
    [paddedCell(SESSION_A, 1, true, mebibyte + mebibyte / 2)],
  );
  expect(harness.scheduler.enqueueTerminalDelta(
    harness.ws,
    SESSION_A,
    snapshotStream,
    paddedCell(SESSION_A, 2, false, mebibyte),
  )).toBe(true);
  const stateStream = "aggregate-byte-state";
  harness.scheduler.beginTerminalStream(harness.ws, TARGET_SESSION, stateStream);
  harness.scheduler.enqueueTerminalState(
    harness.ws,
    paddedState(TARGET_SESSION, stateStream, 3n, mebibyte),
    TARGET_SESSION,
  );

  expect(v2.queuedBytes).toBeLessThan(V2_AGGREGATE_MAX_QUEUED_BYTES);
  expect(v2.queuedBytes).toBeGreaterThan(7 * mebibyte);
  const survivorBytes = workers.queuedBytes + workspaces.queuedBytes;
  const terminalGeneration = harness.terminal.generation;
  harness.scheduler.enqueueTerminalState(
    harness.ws,
    paddedState(TARGET_SESSION, stateStream, 4n, mebibyte),
    TARGET_SESSION,
  );

  expect(harness.terminal.generation).not.toBe(terminalGeneration);
  expect(v2.terminalSessions.size).toBe(0);
  expect(v2.queuedFrames).toBe(2);
  expect(v2.queuedBytes).toBe(survivorBytes);
  expect(workers.queue).toHaveLength(1);
  expect(workspaces.queue).toHaveLength(1);
  expect(decodedFrames(harness.socket).at(-1)?.frame).toMatchObject({
    case: "domainReset",
    value: { domain: SyncDomain.TERMINAL, reason: "aggregate_overflow" },
  });
  expect(harness.socket.closes).toEqual([]);
});
