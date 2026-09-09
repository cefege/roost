// Covers the single baseline-part attach boost within the terminal ready ring.
// It exercises snapshot barriers, view-state fences, and fair continuation rotation.
// The shared scheduler harness supplies deterministic application-ACK backpressure.

import { expect, test } from "bun:test";
import {
  OTHER_SESSION,
  SESSION_A,
  SESSION_B,
  TARGET_SESSION,
  cellIdentity,
  decodedFrames,
  fillApplicationAckWindow,
  flushMicrotasks,
  makeCell,
  makeChunk,
  makeHarness,
  makeState,
  queuedIdentity,
  snapshotSource,
} from "./sync-ws-v2-scheduler-harness.ts";

// A newly attached stream gets one baseline-part boost over foreign deltas,
// but never over another snapshot or ahead of its own view-state fence.

test("a newly attached session's first baseline part jumps foreign queued deltas", async () => {
  const harness = makeHarness("scheduler-test:attach-priority", true);
  const streamA = "stream-attach-a";
  const streamB = "stream-attach-b";
  const streamC = "stream-attach-c";
  harness.scheduler.beginTerminalStream(harness.ws, SESSION_A, streamA);
  harness.scheduler.beginTerminalStream(harness.ws, SESSION_B, streamB);
  for (const sessionId of [SESSION_A, SESSION_B, TARGET_SESSION]) {
    harness.socket.data.v2!.announcedSessions.add(sessionId);
  }
  fillApplicationAckWindow(harness);

  expect(harness.scheduler.enqueueTerminalDelta(harness.ws, SESSION_A, streamA, makeCell(SESSION_A, 1, false))).toBe(true);
  expect(harness.scheduler.enqueueTerminalDelta(harness.ws, SESSION_A, streamA, makeCell(SESSION_A, 2, false))).toBe(true);
  expect(harness.scheduler.enqueueTerminalDelta(harness.ws, SESSION_B, streamB, makeCell(SESSION_B, 3, false))).toBe(true);

  harness.scheduler.beginTerminalStream(harness.ws, TARGET_SESSION, streamC);
  harness.scheduler.replaceTerminalSnapshot(
    harness.ws,
    TARGET_SESSION,
    streamC,
    snapshotSource([makeCell(TARGET_SESSION, 10, true)]),
  );
  await flushMicrotasks();

  expect(harness.socket.sent).toEqual([]);
  expect(harness.terminal.queue.map((item) => queuedIdentity(item.frame))).toEqual([
    { case: "cellGrid", sessionId: TARGET_SESSION, full: true, seq: 10n },
    { case: "cellGrid", sessionId: SESSION_A, full: false, seq: 1n },
    { case: "cellGrid", sessionId: SESSION_B, full: false, seq: 3n },
  ]);
  expect(harness.terminal.queue[0]!.meta.attachSnapshot).toBe(true);
  expect(harness.terminal.queue.slice(1).every((item) => !item.meta.attachSnapshot)).toBe(true);

  const blockedThrough = harness.socket.data.lastSentDeliverySeq;
  expect(harness.delivery.applyCumulativeAck(harness.ws, blockedThrough)).toBe(true);
  await flushMicrotasks();
  const sentCells = decodedFrames(harness.socket).filter(
    (frame) => frame.frame.case === "cellGrid",
  );
  expect(sentCells.map(cellIdentity)).toEqual([
    { sessionId: TARGET_SESSION, full: true, seq: 10n },
    { sessionId: SESSION_A, full: false, seq: 1n },
    { sessionId: SESSION_B, full: false, seq: 3n },
    { sessionId: SESSION_A, full: false, seq: 2n },
  ]);
  expect(harness.delivery.applyCumulativeAck(harness.ws, sentCells.at(-1)!.deliverySeq)).toBe(true);
  await flushMicrotasks();
});

test("an attached baseline never passes another session's queued chunk or full frame", async () => {
  const harness = makeHarness("scheduler-test:snapshot-barrier", true);
  const streamChunk = "stream-barrier-chunk";
  const streamFull = "stream-barrier-full";
  const streamDelta = "stream-barrier-delta";
  const streamAttach = "stream-barrier-attach";
  harness.scheduler.beginTerminalStream(harness.ws, OTHER_SESSION, streamChunk);
  harness.scheduler.replaceTerminalSnapshot(
    harness.ws,
    OTHER_SESSION,
    streamChunk,
    snapshotSource([makeChunk(OTHER_SESSION, 1, 0)]),
  );
  harness.scheduler.beginTerminalStream(harness.ws, SESSION_B, streamFull);
  harness.scheduler.replaceTerminalSnapshot(
    harness.ws,
    SESSION_B,
    streamFull,
    snapshotSource([makeCell(SESSION_B, 5, true)]),
  );
  harness.scheduler.beginTerminalStream(harness.ws, SESSION_A, streamDelta);
  expect(harness.scheduler.enqueueTerminalDelta(harness.ws, SESSION_A, streamDelta, makeCell(SESSION_A, 7, false))).toBe(true);

  harness.scheduler.beginTerminalStream(harness.ws, TARGET_SESSION, streamAttach);
  harness.scheduler.replaceTerminalSnapshot(
    harness.ws,
    TARGET_SESSION,
    streamAttach,
    snapshotSource([makeCell(TARGET_SESSION, 10, true)]),
  );
  await flushMicrotasks();

  expect(harness.terminal.queue.map((item) => queuedIdentity(item.frame))).toEqual([
    { case: "cellGridChunk", sessionId: OTHER_SESSION, chunkIndex: 0 },
    { case: "cellGrid", sessionId: SESSION_B, full: true, seq: 5n },
    { case: "cellGrid", sessionId: TARGET_SESSION, full: true, seq: 10n },
    { case: "cellGrid", sessionId: SESSION_A, full: false, seq: 7n },
  ]);
  expect(harness.socket.sent).toEqual([]);
  expect(harness.socket.closes).toEqual([]);
});

test("an attached baseline keeps its view state ahead of the snapshot", async () => {
  const harness = makeHarness("scheduler-test:attach-session-fifo", true);
  const streamA = "stream-fifo-a";
  const streamB = "stream-fifo-b";
  const streamC = "stream-fifo-c";
  harness.scheduler.beginTerminalStream(harness.ws, SESSION_A, streamA);
  harness.scheduler.beginTerminalStream(harness.ws, SESSION_B, streamB);
  for (const sessionId of [SESSION_A, SESSION_B, TARGET_SESSION]) {
    harness.socket.data.v2!.announcedSessions.add(sessionId);
  }
  fillApplicationAckWindow(harness);

  expect(harness.scheduler.enqueueTerminalDelta(harness.ws, SESSION_A, streamA, makeCell(SESSION_A, 1, false))).toBe(true);
  expect(harness.scheduler.enqueueTerminalDelta(harness.ws, SESSION_B, streamB, makeCell(SESSION_B, 2, false))).toBe(true);
  harness.scheduler.enqueueTerminalState(harness.ws, makeState(TARGET_SESSION, streamC), TARGET_SESSION);
  harness.scheduler.beginTerminalStream(harness.ws, TARGET_SESSION, streamC);
  harness.scheduler.replaceTerminalSnapshot(
    harness.ws,
    TARGET_SESSION,
    streamC,
    snapshotSource([makeCell(TARGET_SESSION, 10, true)]),
  );
  await flushMicrotasks();

  expect(harness.terminal.queue.map((item) => item.frame.frame.case)).toEqual([
    "terminalViewState",
    "cellGrid",
    "cellGrid",
  ]);
  expect(harness.terminal.queue.slice(1).map((item) => cellIdentity(item.frame))).toEqual([
    { sessionId: SESSION_A, full: false, seq: 1n },
    { sessionId: SESSION_B, full: false, seq: 2n },
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
  expect(cellIdentity(sent[1]!)).toEqual({ sessionId: TARGET_SESSION, full: true, seq: 10n });
  expect(sent.slice(2).map(cellIdentity)).toEqual([
    { sessionId: SESSION_A, full: false, seq: 1n },
    { sessionId: SESSION_B, full: false, seq: 2n },
  ]);
  expect(harness.delivery.applyCumulativeAck(harness.ws, sent.at(-1)!.deliverySeq)).toBe(true);
  await flushMicrotasks();
});

test("a snapshot continuation rotates behind steady deltas after its first boosted part", async () => {
  const harness = makeHarness("scheduler-test:attach-continuation", true);
  const streamA = "stream-stale-a";
  const streamC = "stream-stale-c";
  harness.scheduler.beginTerminalStream(harness.ws, SESSION_A, streamA);
  for (const sessionId of [SESSION_A, TARGET_SESSION]) {
    harness.socket.data.v2!.announcedSessions.add(sessionId);
  }
  fillApplicationAckWindow(harness);
  expect(harness.scheduler.enqueueTerminalDelta(harness.ws, SESSION_A, streamA, makeCell(SESSION_A, 1, false))).toBe(true);

  harness.scheduler.beginTerminalStream(harness.ws, TARGET_SESSION, streamC);
  harness.scheduler.replaceTerminalSnapshot(
    harness.ws,
    TARGET_SESSION,
    streamC,
    snapshotSource([
      makeChunk(TARGET_SESSION, 9, 0),
      makeChunk(TARGET_SESSION, 9, 1),
    ]),
  );
  await flushMicrotasks();

  expect(harness.terminal.queue.map((item) => item.meta.attachSnapshot)).toEqual([true, undefined]);
  const blockedThrough = harness.socket.data.lastSentDeliverySeq;
  expect(harness.delivery.applyCumulativeAck(harness.ws, blockedThrough)).toBe(true);
  await flushMicrotasks();

  const sentChunksAndDeltas = decodedFrames(harness.socket).filter(
    (frame) => frame.frame.case === "cellGrid" || frame.frame.case === "cellGridChunk",
  );
  expect(sentChunksAndDeltas.map(queuedIdentity)).toEqual([
    { case: "cellGridChunk", sessionId: TARGET_SESSION, chunkIndex: 0 },
    { case: "cellGrid", sessionId: SESSION_A, full: false, seq: 1n },
    { case: "cellGridChunk", sessionId: TARGET_SESSION, chunkIndex: 1 },
  ]);
  expect(harness.delivery.applyCumulativeAck(harness.ws, decodedFrames(harness.socket).at(-1)!.deliverySeq)).toBe(true);
  await flushMicrotasks();
});
