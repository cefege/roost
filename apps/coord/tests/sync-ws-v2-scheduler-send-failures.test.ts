// This file owns failure-boundary coverage for v1 and v2 scheduler sends.
// Bun's coord test suite runs these cases through the shared scheduler harness.
// It depends on guarded delivery behavior and v2 terminal queue limits.

import { expect, test } from "bun:test";
import { SyncDomain } from "@roost/shared/proto/sync_pb";
import { V2_DOMAIN_MAX_QUEUED_FRAMES } from "../src/connect/sync-ws-v2-state.ts";
import {
  TARGET_SESSION,
  cellIdentity,
  flushMicrotasks,
  makeCell,
  makeHarness,
  makeState,
} from "./sync-ws-v2-scheduler-harness.ts";

test("a throwing application send closes without taking ownership of the queued frame", async () => {
  const harness = makeHarness("scheduler-test:application-send-throw", true);
  const streamId = "stream-application-send-throw";
  harness.scheduler.beginTerminalStream(harness.ws, TARGET_SESSION, streamId);
  harness.socket.data.v2!.announcedSessions.add(TARGET_SESSION);
  harness.socket.sendError = new Error("application send failed");

  expect(
    harness.scheduler.enqueueTerminalDelta(
      harness.ws,
      TARGET_SESSION,
      streamId,
      makeCell(TARGET_SESSION, 7, false),
    ),
  ).toBe(true);
  await flushMicrotasks();

  expect(harness.socket.sendCalls).toBe(1);
  expect(harness.socket.bufferedAmountCalls).toBe(0);
  expect(harness.socket.sent).toEqual([]);
  expect(harness.droppedFrames).toHaveLength(1);
  expect(harness.droppedFrames[0]).toEqual({
    frame: "cellGrid",
    encodedBytes: expect.any(Number),
    bufferedBytes: 0,
  });
  expect(harness.droppedFrames[0]!.encodedBytes).toBeGreaterThan(0);
  expect(harness.socket.data.pressureClosing).toBe(true);
  expect(harness.socket.closes).toEqual([[1013, "sync backpressure"]]);

  expect(harness.terminal.queue).toHaveLength(1);
  expect(cellIdentity(harness.terminal.queue[0]!.frame)).toEqual({
    sessionId: TARGET_SESSION,
    full: false,
    seq: 7n,
  });
  expect(harness.socket.data.v2!.queuedFrames).toBe(1);
  expect(harness.socket.data.lastSentDeliverySeq).toBe(0n);
  expect(harness.socket.data.unackedEncodedBytes).toBe(0);
  expect(harness.socket.data.deliveryQueue).toEqual([]);

  // Closing is the recovery boundary; the ambiguous candidate is never
  // attempted again on the failed socket.
  await flushMicrotasks();
  expect(harness.socket.sendCalls).toBe(1);
});

test("a throwing application buffered-amount probe closes without acknowledging the ambiguous send", async () => {
  const harness = makeHarness("scheduler-test:application-buffered-throw", true);
  const streamId = "stream-application-buffered-throw";
  harness.scheduler.beginTerminalStream(harness.ws, TARGET_SESSION, streamId);
  harness.socket.data.v2!.announcedSessions.add(TARGET_SESSION);
  harness.socket.bufferedAmountError = new Error("buffered amount failed");

  expect(
    harness.scheduler.enqueueTerminalDelta(
      harness.ws,
      TARGET_SESSION,
      streamId,
      makeCell(TARGET_SESSION, 8, false),
    ),
  ).toBe(true);
  await flushMicrotasks();

  expect(harness.socket.sendCalls).toBe(1);
  expect(harness.socket.bufferedAmountCalls).toBe(1);
  expect(harness.socket.sent).toHaveLength(1);
  expect(harness.droppedFrames).toHaveLength(1);
  expect(harness.droppedFrames[0]!.frame).toBe("cellGrid");
  expect(harness.droppedFrames[0]!.encodedBytes).toBeGreaterThan(0);
  expect(harness.droppedFrames[0]!.bufferedBytes).toBe(0);
  expect(harness.socket.data.pressureClosing).toBe(true);
  expect(harness.socket.closes).toEqual([[1013, "sync backpressure"]]);
  expect(harness.terminal.queue).toHaveLength(1);
  expect(harness.socket.data.v2!.queuedFrames).toBe(1);
  expect(harness.socket.data.lastSentDeliverySeq).toBe(0n);
  expect(harness.socket.data.unackedEncodedBytes).toBe(0);
  expect(harness.socket.data.deliveryQueue).toEqual([]);

  await flushMicrotasks();
  expect(harness.socket.sendCalls).toBe(1);
});

test("a throwing control send closes through dropped-frame handling", () => {
  const harness = makeHarness("scheduler-test:control-send-throw", true);
  harness.socket.sendError = new Error("control send failed");

  expect(
    harness.scheduler.sendV2ControlFrame(
      harness.ws,
      makeState(TARGET_SESSION, "stream-control-send-throw"),
    ),
  ).toBe(false);

  expect(harness.socket.sendCalls).toBe(1);
  expect(harness.socket.bufferedAmountCalls).toBe(0);
  expect(harness.socket.sent).toEqual([]);
  expect(harness.droppedFrames).toHaveLength(1);
  expect(harness.droppedFrames[0]!.frame).toBe("terminalViewState");
  expect(harness.droppedFrames[0]!.encodedBytes).toBeGreaterThan(0);
  expect(harness.droppedFrames[0]!.bufferedBytes).toBe(0);
  expect(harness.socket.data.pressureClosing).toBe(true);
  expect(harness.socket.closes).toEqual([[1013, "sync backpressure"]]);
});

test("a throwing control buffered-amount probe closes the ambiguously sent socket", () => {
  const harness = makeHarness("scheduler-test:control-buffered-throw", true);
  harness.socket.bufferedAmountError = new Error("control buffered amount failed");

  expect(
    harness.scheduler.sendV2ControlFrame(
      harness.ws,
      makeState(TARGET_SESSION, "stream-control-buffered-throw"),
    ),
  ).toBe(false);

  expect(harness.socket.sendCalls).toBe(1);
  expect(harness.socket.bufferedAmountCalls).toBe(1);
  expect(harness.socket.sent).toHaveLength(1);
  expect(harness.droppedFrames).toHaveLength(1);
  expect(harness.droppedFrames[0]!.frame).toBe("terminalViewState");
  expect(harness.droppedFrames[0]!.encodedBytes).toBeGreaterThan(0);
  expect(harness.droppedFrames[0]!.bufferedBytes).toBe(0);
  expect(harness.socket.data.pressureClosing).toBe(true);
  expect(harness.socket.closes).toEqual([[1013, "sync backpressure"]]);
});

test("a failed overflow reset send closes the socket with 1013", () => {
  const harness = makeHarness("scheduler-test:reset-send-failure", false);
  for (let seq = 1; seq <= V2_DOMAIN_MAX_QUEUED_FRAMES; seq += 1) {
    expect(harness.scheduler.enqueueV2Frame(
      harness.ws,
      makeCell(TARGET_SESSION, seq, false),
      { domain: SyncDomain.TERMINAL, lane: "cell", sessionId: TARGET_SESSION },
    )).toBe(true);
  }
  harness.socket.sendResult = 0;
  expect(harness.scheduler.enqueueV2Frame(
    harness.ws,
    makeCell(TARGET_SESSION, V2_DOMAIN_MAX_QUEUED_FRAMES + 1, false),
    { domain: SyncDomain.TERMINAL, lane: "cell", sessionId: TARGET_SESSION },
  )).toBe(false);
  expect(harness.droppedFrames.at(-1)?.frame).toBe("domainReset");
  expect(harness.socket.closes).toEqual([[1013, "sync backpressure"]]);
});

test("v1 send and buffer-probe exceptions fail closed with 1013", () => {
  for (const failure of ["send", "buffer"] as const) {
    const harness = makeHarness(`scheduler-test:v1-${failure}-throw`, true);
    if (failure === "send") harness.socket.sendError = new Error("send failed");
    else harness.socket.bufferedAmountError = new Error("buffer failed");
    expect(harness.delivery.sendGuarded(
      harness.ws,
      makeState(TARGET_SESSION, `stream-v1-${failure}`),
    )).toBe(false);
    expect(harness.socket.data.pressureClosing).toBe(true);
    expect(harness.socket.closes).toEqual([[1013, "sync backpressure"]]);
  }
});
