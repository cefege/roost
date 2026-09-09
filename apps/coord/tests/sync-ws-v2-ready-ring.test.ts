// Covers terminal ready-ring fairness, scoped rebaseline ordering, and retry admission.
// It drives the production Sync-v2 scheduler through the deterministic socket harness.
// Idle lanes must own no egress work; canonical replacement remains session-local.

import { expect, test } from "bun:test";
import { SyncDomain } from "@roost/shared/proto/sync_pb";
import {
  V2_TERMINAL_LANE_MAX_DELTA_FRAMES,
  V2_TERMINAL_MAX_RETAINED_FRAMES,
} from "../src/connect/sync-ws-v2-state.ts";
import { APPLICATION_MAX_UNACKED_FRAMES } from "../src/connect/sync-ws-v1-delivery.ts";
import {
  OTHER_SESSION,
  TARGET_SESSION,
  cellIdentity,
  decodedFrames,
  estimatedTerminalBytes,
  flushMicrotasks,
  makeCell,
  makeHarness,
  type SchedulerHarness,
} from "./sync-ws-v2-scheduler-harness.ts";

function idleSessionId(index: number): string {
  return `idle-terminal-lane-${index}`;
}

test("thousands of idle lanes retain no ready work while one hot lane materializes one head", async () => {
  const harness = makeHarness("scheduler-test:ready-ring-idle", false);
  const v2 = harness.socket.data.v2!;
  for (let index = 0; index < 2_000; index++) {
    harness.scheduler.beginTerminalStream(harness.ws, idleSessionId(index), `stream-idle-${index}`);
  }

  expect(v2.terminalSessions.size).toBe(2_000);
  expect(v2.terminalReadySessions.size).toBe(0);
  const hotSessionId = "hot-terminal-lane";
  const hotStreamId = "stream-hot-terminal-lane";
  harness.scheduler.beginTerminalStream(harness.ws, hotSessionId, hotStreamId);
  expect(harness.scheduler.enqueueTerminalDelta(
    harness.ws,
    hotSessionId,
    hotStreamId,
    makeCell(hotSessionId, 1, false),
  )).toBe(true);
  await flushMicrotasks();

  expect(harness.terminal.queue.map((item) => cellIdentity(item.frame))).toEqual([
    { sessionId: hotSessionId, full: false, seq: 1n },
  ]);
  expect(v2.terminalReadySessions.size).toBe(0);
  expect(v2.terminalRetainedFrames).toBe(1);
  for (const sessionId of v2.terminalSessions.keys()) {
    if (sessionId === hotSessionId) continue;
    expect(v2.terminalSessions.get(sessionId)?.ready).toBe(false);
  }
});

test("a lane overflow preserves an active snapshot, then installs one scoped canonical full", async () => {
  const targetStreamId = "stream-scoped-target";
  const otherStreamId = "stream-scoped-other";
  const firstSnapshotPart = makeCell(TARGET_SESSION, 1, true);
  const finalSnapshotPart = makeCell(TARGET_SESSION, 2, false);
  const replacementFull = makeCell(TARGET_SESSION, 100, true);
  const otherDelta = makeCell(OTHER_SESSION, 1, false);
  let harness!: SchedulerHarness;
  harness = makeHarness("scheduler-test:scoped-overflow", true, {
    requestTerminalRebaseline(sessionId) {
      if (sessionId !== TARGET_SESSION) return false;
      return harness.scheduler.replaceTerminalSnapshot(
        harness.ws,
        TARGET_SESSION,
        targetStreamId,
        [replacementFull],
      );
    },
  });
  const v2 = harness.socket.data.v2!;
  const generation = harness.terminal.generation;
  v2.announcedSessions.add(TARGET_SESSION);
  v2.announcedSessions.add(OTHER_SESSION);
  harness.scheduler.beginTerminalStream(harness.ws, TARGET_SESSION, targetStreamId);
  harness.scheduler.beginTerminalStream(harness.ws, OTHER_SESSION, otherStreamId);
  harness.socket.afterBufferedAmount = () => {
    for (let index = 0; index < APPLICATION_MAX_UNACKED_FRAMES; index++) {
      harness.socket.data.deliveryQueue.push({
        seq: BigInt(10_000 + index),
        encodedBytes: 0,
        sentAtMs: 0,
      });
    }
  };
  expect(harness.scheduler.replaceTerminalSnapshot(
    harness.ws,
    TARGET_SESSION,
    targetStreamId,
    [firstSnapshotPart, finalSnapshotPart],
  )).toBe(true);
  expect(harness.scheduler.enqueueTerminalDelta(
    harness.ws,
    OTHER_SESSION,
    otherStreamId,
    otherDelta,
  )).toBe(true);
  await flushMicrotasks();

  const targetLane = v2.terminalSessions.get(TARGET_SESSION)!;
  expect(decodedFrames(harness.socket).map(cellIdentity)).toEqual([
    { sessionId: TARGET_SESSION, full: true, seq: 1n },
  ]);
  expect(targetLane.cursor?.index).toBe(1);
  expect(targetLane.cursor?.queued).toBe(true);
  for (let sequence = 3; sequence < 3 + V2_TERMINAL_LANE_MAX_DELTA_FRAMES; sequence++) {
    expect(harness.scheduler.enqueueTerminalDelta(
      harness.ws,
      TARGET_SESSION,
      targetStreamId,
      makeCell(TARGET_SESSION, sequence, false),
    )).toBe(true);
  }
  expect(harness.scheduler.enqueueTerminalDelta(
    harness.ws,
    TARGET_SESSION,
    targetStreamId,
    makeCell(TARGET_SESSION, 3 + V2_TERMINAL_LANE_MAX_DELTA_FRAMES, false),
  )).toBe(false);

  const retainedBeforeDrain = estimatedTerminalBytes(finalSnapshotPart, generation)
    + estimatedTerminalBytes(otherDelta, generation);
  expect(targetLane.rebaselinePending).toBe(true);
  expect(targetLane.cursor?.deltaTail).toEqual([]);
  expect(harness.rebaselineRequests).toEqual([]);
  expect(v2.terminalRetainedFrames).toBe(2);
  expect(v2.terminalRetainedBytes).toBe(retainedBeforeDrain);
  expect(harness.terminal.generation).toBe(generation);

  harness.socket.data.deliveryQueue.length = 0;
  harness.socket.data.unackedEncodedBytes = 0;
  harness.socket.data.deliveryTimer = null;
  harness.scheduler.scheduleV2(harness.ws);
  await flushMicrotasks();

  expect(decodedFrames(harness.socket).map(cellIdentity)).toEqual([
    { sessionId: TARGET_SESSION, full: true, seq: 1n },
    { sessionId: OTHER_SESSION, full: false, seq: 1n },
    { sessionId: TARGET_SESSION, full: false, seq: 2n },
    { sessionId: TARGET_SESSION, full: true, seq: 100n },
  ]);
  expect(harness.rebaselineRequests).toEqual([TARGET_SESSION]);
  expect(harness.terminal.generation).toBe(generation);
  expect(v2.terminalSessions.get(OTHER_SESSION)?.streamId).toBe(otherStreamId);
  expect(v2.terminalRetainedFrames).toBe(0);
  expect(v2.terminalRetainedBytes).toBe(0);
  expect(v2.queuedFrames).toBe(0);
  expect(v2.queuedBytes).toBe(0);
});

test("a rejected canonical full stays ready until later egress frees terminal credit", async () => {
  const replacementStreamId = "stream-deferred-rebaseline";
  const replacementFull = makeCell(TARGET_SESSION, 1_000, true);
  let harness!: SchedulerHarness;
  harness = makeHarness("scheduler-test:deferred-rebaseline", false, {
    requestTerminalRebaseline(sessionId) {
      if (sessionId !== TARGET_SESSION) return false;
      return harness.scheduler.replaceTerminalSnapshot(
        harness.ws,
        TARGET_SESSION,
        replacementStreamId,
        [replacementFull],
      );
    },
  });
  const v2 = harness.socket.data.v2!;
  const generation = harness.terminal.generation;
  for (let index = 0; index < V2_TERMINAL_MAX_RETAINED_FRAMES; index++) {
    const sessionId = idleSessionId(index);
    v2.announcedSessions.add(sessionId);
    expect(harness.scheduler.enqueueV2Frame(
      harness.ws,
      makeCell(sessionId, 1, false),
      { domain: SyncDomain.TERMINAL, lane: "cell", sessionId },
    )).toBe(true);
  }
  v2.announcedSessions.add(TARGET_SESSION);
  harness.scheduler.beginTerminalStream(harness.ws, TARGET_SESSION, replacementStreamId);
  expect(harness.scheduler.replaceTerminalSnapshot(
    harness.ws,
    TARGET_SESSION,
    replacementStreamId,
    [replacementFull],
  )).toBe(false);
  expect(v2.terminalSessions.get(TARGET_SESSION)?.rebaselinePending).toBe(true);
  expect(v2.terminalReadySessions.has(TARGET_SESSION)).toBe(true);
  expect(v2.terminalRetainedFrames).toBe(V2_TERMINAL_MAX_RETAINED_FRAMES);

  harness.terminal.ready = true;
  harness.scheduler.scheduleV2(harness.ws);
  await flushMicrotasks();

  expect(harness.rebaselineRequests).toEqual([TARGET_SESSION, TARGET_SESSION]);
  expect(harness.terminal.generation).toBe(generation);
  expect(v2.terminalSessions.get(TARGET_SESSION)?.rebaselinePending).toBe(false);
  expect(decodedFrames(harness.socket).some((frame) => frame.frame.case === "domainReset")).toBe(false);
});
