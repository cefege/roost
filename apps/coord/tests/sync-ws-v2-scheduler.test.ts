import { expect, test } from "bun:test";
import { SyncDomain, type FirehoseFrame } from "@roost/shared/proto/sync_pb";
import { V2_DOMAIN_MAX_QUEUED_FRAMES } from "../src/connect/sync-ws-v2-state.ts";
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

  const survivors = [other, newFull];
  const survivorBytes = survivors.map((frame) =>
    estimatedTerminalBytes(frame, harness.terminal.generation)
  );
  expect(harness.terminal.queue.map((item) => cellIdentity(item.frame))).toEqual([
    { sessionId: OTHER_SESSION, full: false, seq: 1n },
    { sessionId: TARGET_SESSION, full: true, seq: 4n },
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
