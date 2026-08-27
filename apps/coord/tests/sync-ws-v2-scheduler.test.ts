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
  V2_DOMAIN_MAX_QUEUED_FRAMES,
  clearV2State,
} from "../src/connect/sync-ws-v2-state.ts";
import {
  OTHER_SESSION,
  SESSION_A,
  SESSION_B,
  TARGET_SESSION,
  cellIdentity,
  decodedFrames,
  estimatedTerminalBytes,
  fillApplicationAckWindow,
  flushMicrotasks,
  makeCell,
  makeHarness,
  makeState,
} from "./sync-ws-v2-scheduler-harness.ts";
test("dropping a terminal stream prunes its backlog before a replacement snapshot reaches the queue limit", async () => {
  const harness = makeHarness("scheduler-test:overflow-prune", true);
  const streamA = "stream-a";
  const streamB = "stream-b";
  harness.scheduler.beginTerminalStream(harness.ws, SESSION_A, streamA);
  harness.socket.data.v2!.announcedSessions.add(SESSION_A);
  fillApplicationAckWindow(harness);
  const queuedA: FirehoseFrame[] = [];
  for (let seq = 1; seq <= V2_DOMAIN_MAX_QUEUED_FRAMES; seq += 1) {
    const frame = makeCell(SESSION_A, seq, false);
    queuedA.push(frame);
    expect(harness.scheduler.enqueueTerminalDelta(harness.ws, SESSION_A, streamA, frame)).toBe(true);
  }
  const queuedABytes = queuedA.reduce(
    (total, frame) => total + estimatedTerminalBytes(frame, harness.terminal.generation),
    0,
  );
  expect(harness.terminal.queue).toHaveLength(V2_DOMAIN_MAX_QUEUED_FRAMES);
  expect(harness.terminal.queuedBytes).toBe(queuedABytes);
  expect(harness.socket.data.v2!.queuedFrames).toBe(V2_DOMAIN_MAX_QUEUED_FRAMES);
  expect(harness.socket.data.v2!.queuedBytes).toBe(queuedABytes);
  expect(harness.socket.closes).toEqual([]);
  await flushMicrotasks();
  expect(harness.socket.data.v2!.schedulerPending).toBe(false);
  expect(harness.socket.sent).toEqual([]);
  harness.scheduler.dropTerminalSession(harness.ws, SESSION_A);
  harness.scheduler.beginTerminalStream(harness.ws, SESSION_B, streamB);
  harness.socket.data.v2!.announcedSessions.add(SESSION_B);
  const replacement = makeCell(SESSION_B, 10_000, true);
  const replacementBytes = estimatedTerminalBytes(replacement, harness.terminal.generation);
  harness.scheduler.replaceTerminalSnapshot(harness.ws, SESSION_B, streamB, [replacement]);
  expect(harness.socket.closes).toEqual([]);
  expect(harness.terminal.queue.map((item) => cellIdentity(item.frame))).toEqual([{
    sessionId: SESSION_B,
    full: true,
    seq: 10_000n,
  }]);
  expect(harness.terminal.queue.some((item) => item.meta.sessionId === SESSION_A)).toBe(false);
  expect(harness.terminal.queue[0]!.estimatedBytes).toBe(replacementBytes);
  expect(harness.terminal.queuedBytes).toBe(replacementBytes);
  expect(harness.socket.data.v2!.queuedFrames).toBe(1);
  expect(harness.socket.data.v2!.queuedBytes).toBe(replacementBytes);
  const blockedThrough = harness.socket.data.lastSentDeliverySeq;
  expect(harness.delivery.applyCumulativeAck(harness.ws, blockedThrough)).toBe(true);
  await flushMicrotasks();
  const sentCells = decodedFrames(harness.socket).filter(
    (frame) => frame.frame.case === "cellGrid",
  );
  expect(sentCells.map(cellIdentity)).toEqual([{
    sessionId: SESSION_B,
    full: true,
    seq: 10_000n,
  }]);
  expect(sentCells[0]!.domain).toBe(SyncDomain.TERMINAL);
  expect(sentCells[0]!.domainGeneration).toBe(harness.terminal.generation);
  expect(sentCells[0]!.deliverySeq).toBe(blockedThrough + 1n);
  expect(harness.terminal.queue).toEqual([]);
  expect(harness.terminal.queuedBytes).toBe(0);
  expect(harness.socket.data.v2!.queuedFrames).toBe(0);
  expect(harness.socket.data.v2!.queuedBytes).toBe(0);
  expect(harness.socket.closes).toEqual([]);
  expect(harness.delivery.applyCumulativeAck(harness.ws, sentCells[0]!.deliverySeq)).toBe(true);
  await flushMicrotasks();
});
test("a replacement snapshot supersedes only its own stream cursor and delta tail", async () => {
  const harness = makeHarness("scheduler-test:snapshot-supersession", false);
  const otherStream = "stream-other";
  const targetStream = "stream-target";
  harness.scheduler.beginTerminalStream(harness.ws, OTHER_SESSION, otherStream);
  harness.scheduler.beginTerminalStream(harness.ws, TARGET_SESSION, targetStream);
  const other = makeCell(OTHER_SESSION, 1, false);
  const oldFull = makeCell(TARGET_SESSION, 2, true);
  const oldSnapshotTail = makeCell(TARGET_SESSION, 3, false);
  const oldDeltaTail = makeCell(TARGET_SESSION, 30, false);
  const newFull = makeCell(TARGET_SESSION, 4, true);
  const followingDelta = makeCell(TARGET_SESSION, 5, false);
  expect(harness.scheduler.enqueueTerminalDelta(harness.ws, OTHER_SESSION, otherStream, other)).toBe(true);
  harness.scheduler.replaceTerminalSnapshot(harness.ws, TARGET_SESSION, targetStream, [oldFull, oldSnapshotTail]);
  expect(harness.scheduler.enqueueTerminalDelta(harness.ws, TARGET_SESSION, targetStream, oldDeltaTail)).toBe(true);
  harness.scheduler.replaceTerminalSnapshot(harness.ws, TARGET_SESSION, targetStream, [newFull]);
  expect(harness.scheduler.enqueueTerminalDelta(harness.ws, TARGET_SESSION, targetStream, followingDelta)).toBe(true);
  await flushMicrotasks();
  // The fresh TARGET baseline jumps the OTHER delta; snapshots never pass
  // snapshots and per-session FIFO still holds.
  const survivors = [newFull, other];
  const survivorBytes = survivors.map((frame) =>
    estimatedTerminalBytes(frame, harness.terminal.generation)
  );
  expect(harness.terminal.queue.map((item) => cellIdentity(item.frame))).toEqual([
    { sessionId: TARGET_SESSION, full: true, seq: 4n },
    { sessionId: OTHER_SESSION, full: false, seq: 1n },
  ]);
  expect(harness.terminal.queue.map((item) => item.estimatedBytes)).toEqual(survivorBytes);
  const cursor = harness.socket.data.v2!.terminalSessions.get(TARGET_SESSION)?.cursor;
  expect(cursor?.frames.map(cellIdentity)).toEqual([
    { sessionId: TARGET_SESSION, full: true, seq: 4n },
  ]);
  expect(cursor?.deltaTail.map(cellIdentity)).toEqual([
    { sessionId: TARGET_SESSION, full: false, seq: 5n },
  ]);
  expect(cursor?.index).toBe(0);
  expect(cursor?.queued).toBe(true);
  expect(harness.terminal.queuedBytes).toBe(
    survivorBytes.reduce((total, bytes) => total + bytes, 0),
  );
  expect(harness.socket.data.v2!.queuedFrames).toBe(2);
  expect(harness.socket.data.v2!.queuedBytes).toBe(harness.terminal.queuedBytes);
  expect(harness.socket.data.v2!.schedulerPending).toBe(false);
  expect(harness.socket.sent).toEqual([]);
  expect(harness.socket.closes).toEqual([]);
});
test("delta-only terminal streams retain FIFO order without a snapshot cursor", async () => {
  const harness = makeHarness("scheduler-test:delta-fifo", false);
  const streamId = "stream-delta-fifo";
  harness.scheduler.beginTerminalStream(harness.ws, TARGET_SESSION, streamId);
  const deltas = [
    makeCell(TARGET_SESSION, 11, false),
    makeCell(TARGET_SESSION, 12, false),
    makeCell(TARGET_SESSION, 13, false),
  ];
  for (const delta of deltas) {
    expect(harness.scheduler.enqueueTerminalDelta(harness.ws, TARGET_SESSION, streamId, delta)).toBe(true);
  }
  await flushMicrotasks();
  const expectedBytes = deltas.map((frame) =>
    estimatedTerminalBytes(frame, harness.terminal.generation)
  );
  expect(harness.terminal.queue.map((item) => cellIdentity(item.frame))).toEqual([
    { sessionId: TARGET_SESSION, full: false, seq: 11n },
    { sessionId: TARGET_SESSION, full: false, seq: 12n },
    { sessionId: TARGET_SESSION, full: false, seq: 13n },
  ]);
  expect(harness.terminal.queue.map((item) => item.estimatedBytes)).toEqual(expectedBytes);
  expect(harness.socket.data.v2!.terminalSessions.get(TARGET_SESSION)?.cursor).toBeNull();
  expect(harness.terminal.queuedBytes).toBe(
    expectedBytes.reduce((total, bytes) => total + bytes, 0),
  );
  expect(harness.socket.data.v2!.queuedFrames).toBe(3);
  expect(harness.socket.data.v2!.queuedBytes).toBe(harness.terminal.queuedBytes);
  expect(harness.socket.data.v2!.schedulerPending).toBe(false);
  expect(harness.socket.sent).toEqual([]);
  expect(harness.socket.closes).toEqual([]);
});
test("capped scheduler batches yield once before draining the next 64 frames", async () => {
  const harness = makeHarness("scheduler-test:capped-yield", true);
  const streamId = "stream-capped-yield";
  harness.socket.data.v2!.announcedSessions.add(TARGET_SESSION);
  harness.scheduler.beginTerminalStream(harness.ws, TARGET_SESSION, streamId);
  for (let seq = 1; seq <= 128; seq += 1) {
    expect(
      harness.scheduler.enqueueTerminalDelta(
        harness.ws,
        TARGET_SESSION,
        streamId,
        makeCell(TARGET_SESSION, seq, false),
      ),
    ).toBe(true);
  }
  await flushMicrotasks();
  const firstBatch = decodedFrames(harness.socket)
    .filter((frame) => frame.frame.case === "cellGrid");
  expect(firstBatch).toHaveLength(64);
  expect(firstBatch.map(cellIdentity).map((cell) => cell.seq)).toEqual(
    Array.from({ length: 64 }, (_, index) => BigInt(index + 1)),
  );
  const firstContinuation = harness.socket.data.v2!.schedulerYieldTimer;
  expect(firstContinuation).not.toBeNull();
  expect(harness.clock.pendingTimerCount(0)).toBe(1);
  // New data and an ACK may arrive while the macrotask yield is pending, but
  // neither is allowed to bypass it or create a second continuation.
  expect(
    harness.scheduler.enqueueTerminalDelta(
      harness.ws,
      TARGET_SESSION,
      streamId,
      makeCell(TARGET_SESSION, 129, false),
    ),
  ).toBe(true);
  expect(harness.delivery.applyCumulativeAck(harness.ws, 64n)).toBe(true);
  expect(
    harness.scheduler.sendV2ControlFrame(harness.ws, makeState(TARGET_SESSION, streamId)),
  ).toBe(true);
  await flushMicrotasks();
  expect(decodedFrames(harness.socket)).toHaveLength(65);
  expect(harness.socket.data.v2!.schedulerYieldTimer).toBe(firstContinuation);
  expect(harness.clock.pendingTimerCount(0)).toBe(1);
  expect(harness.clock.runNextZeroDelayTimer()).toBe(true);
  await flushMicrotasks();
  const secondTurn = decodedFrames(harness.socket);
  expect(secondTurn).toHaveLength(129);
  expect(secondTurn[64]!.frame.case).toBe("terminalViewState");
  expect(secondTurn.filter((frame) => frame.frame.case === "cellGrid")).toHaveLength(128);
  expect(
    secondTurn
      .filter((frame) => frame.frame.case === "cellGrid")
      .map(cellIdentity)
      .map((cell) => cell.seq),
  ).toEqual(Array.from({ length: 128 }, (_, index) => BigInt(index + 1)));
  expect(harness.clock.pendingTimerCount(0)).toBe(1);
  expect(harness.socket.data.v2!.schedulerYieldTimer).not.toBeNull();

  expect(harness.clock.runNextZeroDelayTimer()).toBe(true);
  await flushMicrotasks();
  const finalFrames = decodedFrames(harness.socket);
  expect(finalFrames.filter((frame) => frame.frame.case === "cellGrid")).toHaveLength(129);
  expect(finalFrames.at(-1)!.frame.case).toBe("cellGrid");
  expect(cellIdentity(finalFrames.at(-1)!)).toEqual({
    sessionId: TARGET_SESSION,
    full: false,
    seq: 129n,
  });
  expect(harness.socket.data.v2!.schedulerYieldTimer).toBeNull();
  expect(harness.clock.pendingTimerCount(0)).toBe(0);
  expect(harness.socket.data.v2!.schedulerPending).toBe(false);
});

test("a reset cancels a capped continuation and retires its stale callback", async () => {
  const harness = makeHarness("scheduler-test:capped-yield-reset", true);
  const streamId = "stream-capped-yield-reset";
  harness.scheduler.beginTerminalStream(harness.ws, TARGET_SESSION, streamId);
  harness.socket.data.v2!.announcedSessions.add(TARGET_SESSION);
  for (let seq = 1; seq <= 65; seq += 1) {
    expect(
      harness.scheduler.enqueueTerminalDelta(
        harness.ws,
        TARGET_SESSION,
        streamId,
        makeCell(TARGET_SESSION, seq, false),
      ),
    ).toBe(true);
  }
  await flushMicrotasks();

  const staleContinuation = harness.socket.data.v2!.schedulerYieldTimer;
  expect(staleContinuation).not.toBeNull();
  harness.socket.data.pressureClosing = true;
  clearV2State(harness.ws, (timer) => harness.clock.clearTimeout(timer));
  expect(harness.socket.data.v2!.schedulerYieldTimer).toBeNull();
  expect(harness.clock.pendingTimerCount(0)).toBe(0);

  // Exercise a callback that was already taken from the clock's queue before
  // cleanup; the timer identity guard must leave the cleared state inert.
  expect(harness.clock.invokeTimerCallback(staleContinuation!)).toBe(true);
  await flushMicrotasks();
  expect(harness.socket.sent).toHaveLength(64);
  expect(harness.socket.data.v2!.schedulerPending).toBe(false);
  expect(harness.socket.data.v2!.queuedFrames).toBe(0);
});

test("terminal state precedes the snapshot cursor and its delta tail after ACK restart", async () => {
  const harness = makeHarness("scheduler-test:state-cursor-order", true);
  const streamId = "stream-state-cursor";
  harness.scheduler.beginTerminalStream(harness.ws, TARGET_SESSION, streamId);
  harness.socket.data.v2!.announcedSessions.add(TARGET_SESSION);
  fillApplicationAckWindow(harness);

  const state = makeState(TARGET_SESSION, streamId);
  const snapshot = [
    makeCell(TARGET_SESSION, 20, true),
    makeCell(TARGET_SESSION, 21, false),
  ];
  const deltaTail = makeCell(TARGET_SESSION, 22, false);
  harness.scheduler.enqueueTerminalState(harness.ws, state, TARGET_SESSION);
  harness.scheduler.replaceTerminalSnapshot(harness.ws, TARGET_SESSION, streamId, snapshot);
  expect(harness.scheduler.enqueueTerminalDelta(harness.ws, TARGET_SESSION, streamId, deltaTail)).toBe(true);
  await flushMicrotasks();

  expect(harness.socket.sent).toEqual([]);
  expect(harness.terminal.queue.map((item) => item.frame.frame.case)).toEqual([
    "terminalViewState",
    "cellGrid",
  ]);
  const blockedCursor = harness.socket.data.v2!.terminalSessions.get(TARGET_SESSION)?.cursor;
  expect(blockedCursor?.index).toBe(0);
  expect(blockedCursor?.queued).toBe(true);
  expect(blockedCursor?.deltaTail.map(cellIdentity)).toEqual([
    { sessionId: TARGET_SESSION, full: false, seq: 22n },
  ]);

  const blockedThrough = harness.socket.data.lastSentDeliverySeq;
  expect(harness.delivery.applyCumulativeAck(harness.ws, blockedThrough)).toBe(true);
  await flushMicrotasks();

  const sent = decodedFrames(harness.socket);
  expect(sent.map((frame) => frame.frame.case)).toEqual([
    "terminalViewState",
    "cellGrid",
    "cellGrid",
    "cellGrid",
  ]);
  expect(sent.slice(1).map(cellIdentity)).toEqual([
    { sessionId: TARGET_SESSION, full: true, seq: 20n },
    { sessionId: TARGET_SESSION, full: false, seq: 21n },
    { sessionId: TARGET_SESSION, full: false, seq: 22n },
  ]);
  expect(sent.map((frame) => frame.deliverySeq)).toEqual([
    blockedThrough + 1n,
    blockedThrough + 2n,
    blockedThrough + 3n,
    blockedThrough + 4n,
  ]);
  expect(harness.socket.data.v2!.terminalSessions.get(TARGET_SESSION)?.cursor).toBeNull();
  expect(harness.terminal.queue).toEqual([]);
  expect(harness.socket.closes).toEqual([]);

  expect(harness.delivery.applyCumulativeAck(harness.ws, sent.at(-1)!.deliverySeq)).toBe(true);
  await flushMicrotasks();
});

// Distinct revisions make the shed order observable: with a blocked domain
// queue, the backlog must keep the NEWEST states at the frame cap.
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

test("a blocked terminal domain sheds pendingStates oldest-first at the frame cap", () => {
  const harness = makeHarness("scheduler-test:pending-states-overflow", true);
  const streamId = "stream-pending-overflow";
  const pushed = V2_DOMAIN_MAX_QUEUED_FRAMES + 1;

  // Saturate the TERMINAL domain queue so every state push below stalls in
  // the lane's pendingStates backlog instead of draining into the queue.
  harness.scheduler.beginTerminalStream(harness.ws, SESSION_A, streamId);
  harness.socket.data.v2!.announcedSessions.add(SESSION_A);
  fillApplicationAckWindow(harness);
  for (let seq = 1; seq <= V2_DOMAIN_MAX_QUEUED_FRAMES; seq += 1) {
    expect(
      harness.scheduler.enqueueTerminalDelta(
        harness.ws, SESSION_A, streamId, makeCell(SESSION_A, seq, false),
      ),
    ).toBe(true);
  }
  expect(harness.terminal.queue).toHaveLength(V2_DOMAIN_MAX_QUEUED_FRAMES);
  for (let seq = 1; seq <= pushed; seq += 1) {
    harness.scheduler.enqueueTerminalState(
      harness.ws,
      pendingState(SESSION_A, streamId, BigInt(seq)),
      SESSION_A,
    );
  }

  const lane = harness.socket.data.v2!.terminalSessions.get(SESSION_A);
  const revisions = (lane?.pendingStates ?? []).map(
    (frame) => frame.frame.case === "terminalViewState"
      ? frame.frame.value.revision
      : undefined,
  );
  expect(lane?.pendingStates).toHaveLength(V2_DOMAIN_MAX_QUEUED_FRAMES);
  expect(revisions[0]).toBe(2n);
  expect(revisions.at(-1)).toBe(BigInt(pushed));
  // The saturated domain queue must be untouched — the cap is shed inside the
  // lane's own backlog, never by resetting the shared queue.
  expect(harness.socket.closes).toEqual([]);
});

