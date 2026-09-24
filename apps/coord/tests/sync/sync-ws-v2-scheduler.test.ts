// Covers Sync-v2 terminal cursor ordering independent of pressure recovery.
// Ready-ring fairness and scoped rebaseline cases live beside this suite.
// The shared harness exposes real queue ownership and application ACK behavior.

import { expect, test } from "bun:test";
import {
  OTHER_SESSION,
  TARGET_SESSION,
  cellIdentity,
  decodedFrames,
  fillApplicationAckWindow,
  flushMicrotasks,
  makeCell,
  makeHarness,
  makeState,
  snapshotSource,
} from "./sync-ws-v2-scheduler-harness.ts";

test("a replacement snapshot supersedes only its unsent cursor and delta tail", async () => {
  const harness = makeHarness("scheduler-test:snapshot-supersession", false);
  const otherStream = "stream-other";
  const targetStream = "stream-target";
  const other = makeCell(OTHER_SESSION, 1, false);
  const oldFull = makeCell(TARGET_SESSION, 2, true);
  const oldTail = makeCell(TARGET_SESSION, 3, false);
  const oldDelta = makeCell(TARGET_SESSION, 30, false);
  const replacement = makeCell(TARGET_SESSION, 4, true);
  const followingDelta = makeCell(TARGET_SESSION, 5, false);
  harness.scheduler.beginTerminalStream(harness.ws, OTHER_SESSION, otherStream);
  harness.scheduler.beginTerminalStream(harness.ws, TARGET_SESSION, targetStream);
  expect(harness.scheduler.enqueueTerminalDelta(harness.ws, OTHER_SESSION, otherStream, other)).toBe(true);
  expect(harness.scheduler.replaceTerminalSnapshot(
    harness.ws,
    TARGET_SESSION,
    targetStream,
    snapshotSource([oldFull, oldTail]),
  )).toBe(true);
  expect(harness.scheduler.enqueueTerminalDelta(
    harness.ws,
    TARGET_SESSION,
    targetStream,
    oldDelta,
  )).toBe(true);
  expect(harness.scheduler.replaceTerminalSnapshot(
    harness.ws,
    TARGET_SESSION,
    targetStream,
    snapshotSource([replacement]),
  )).toBe(true);
  expect(harness.scheduler.enqueueTerminalDelta(
    harness.ws,
    TARGET_SESSION,
    targetStream,
    followingDelta,
  )).toBe(true);
  await flushMicrotasks();

  expect(harness.terminal.queue.map((item) => cellIdentity(item.frame))).toEqual([
    { sessionId: TARGET_SESSION, full: true, seq: 4n },
    { sessionId: OTHER_SESSION, full: false, seq: 1n },
  ]);
  const cursor = harness.socket.data.v2!.terminalSessions.get(TARGET_SESSION)!.cursor!;
  expect(cursor.source?.partCount).toBe(1);
  expect(cellIdentity(cursor.materialized!.frame)).toEqual({
    sessionId: TARGET_SESSION,
    full: true,
    seq: 4n,
  });
  expect(cursor.deltaTail.map((item) => cellIdentity(item.frame))).toEqual([
    { sessionId: TARGET_SESSION, full: false, seq: 5n },
  ]);
  expect(harness.socket.data.v2!.terminalRetainedFrames).toBe(3);
  expect(harness.socket.data.v2!.queuedFrames).toBe(3);
});

test("a delta-only lane exposes one head and drains its retained FIFO in order", async () => {
  const harness = makeHarness("scheduler-test:delta-fifo", false);
  const streamId = "stream-delta-fifo";
  const deltas = [11, 12, 13].map((sequence) => makeCell(TARGET_SESSION, sequence, false));
  harness.scheduler.beginTerminalStream(harness.ws, TARGET_SESSION, streamId);
  for (const delta of deltas) {
    expect(harness.scheduler.enqueueTerminalDelta(harness.ws, TARGET_SESSION, streamId, delta)).toBe(true);
  }
  await flushMicrotasks();

  expect(harness.terminal.queue.map((item) => cellIdentity(item.frame))).toEqual([
    { sessionId: TARGET_SESSION, full: false, seq: 11n },
  ]);
  const cursor = harness.socket.data.v2!.terminalSessions.get(TARGET_SESSION)!.cursor!;
  expect(cursor.deltaTail.map((item) => cellIdentity(item.frame))).toEqual([
    { sessionId: TARGET_SESSION, full: false, seq: 11n },
    { sessionId: TARGET_SESSION, full: false, seq: 12n },
    { sessionId: TARGET_SESSION, full: false, seq: 13n },
  ]);
  expect(harness.socket.data.v2!.terminalRetainedFrames).toBe(3);

  harness.terminal.ready = true;
  harness.socket.data.v2!.announcedSessions.add(TARGET_SESSION);
  harness.scheduler.scheduleV2(harness.ws);
  await flushMicrotasks();
  expect(decodedFrames(harness.socket).map(cellIdentity)).toEqual([
    { sessionId: TARGET_SESSION, full: false, seq: 11n },
    { sessionId: TARGET_SESSION, full: false, seq: 12n },
    { sessionId: TARGET_SESSION, full: false, seq: 13n },
  ]);
  expect(harness.socket.data.v2!.terminalRetainedFrames).toBe(0);
  expect(harness.socket.data.v2!.queuedFrames).toBe(0);
});

test("terminal state precedes snapshot parts and delta tail after an ACK restart", async () => {
  const harness = makeHarness("scheduler-test:state-cursor-order", true);
  const streamId = "stream-state-cursor";
  harness.scheduler.beginTerminalStream(harness.ws, TARGET_SESSION, streamId);
  harness.socket.data.v2!.announcedSessions.add(TARGET_SESSION);
  fillApplicationAckWindow(harness);
  harness.scheduler.enqueueTerminalState(harness.ws, makeState(TARGET_SESSION, streamId), TARGET_SESSION);
  expect(harness.scheduler.replaceTerminalSnapshot(
    harness.ws,
    TARGET_SESSION,
    streamId,
    snapshotSource([makeCell(TARGET_SESSION, 20, true), makeCell(TARGET_SESSION, 21, false)]),
  )).toBe(true);
  expect(harness.scheduler.enqueueTerminalDelta(
    harness.ws,
    TARGET_SESSION,
    streamId,
    makeCell(TARGET_SESSION, 22, false),
  )).toBe(true);
  await flushMicrotasks();

  expect(harness.terminal.queue.map((item) => item.frame.frame.case)).toEqual([
    "terminalViewState",
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
  expect(harness.socket.data.v2!.terminalSessions.get(TARGET_SESSION)?.cursor).toBeNull();
  expect(harness.socket.data.v2!.queuedFrames).toBe(0);
});
