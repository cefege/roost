// Exercises worker-owned raw-metadata staging and fair dispatch.
// Builds a minimal SessionManager without a terminal-core dependency.
// Covers ordering, wake ownership, bounds, drop isolation, and teardown.

import { afterEach, describe, expect, test, vi } from "bun:test";
import { asWorkerFp, DIR_FROM_PTY } from "@roost/shared/wire";
import {
  CELL_EMIT_COALESCE_MS,
  RAW_METADATA_AGGREGATE_CAP_BYTES,
  RAW_METADATA_CHANNEL_CAP_BYTES,
} from "../src/session-constants.ts";
import {
  disposeRawMetadataState,
  RAW_METADATA_DISPATCH_FRAME_BUDGET,
} from "../src/session-raw-metadata.ts";
import { SessionManager } from "../src/session-manager.ts";
import type { TransportSendResult } from "../src/transport/coord-link-types.ts";
import { SessionEventTestSink } from "./session-event-test-sink.ts";

const managers = new Set<SessionManager>();

type RawSend = (
  channelId: number,
  direction: number,
  endSeq: number,
  bytes: Uint8Array,
) => TransportSendResult;

function makeManager(send: RawSend): SessionManager {
  const manager = new SessionManager({
    workerFp: asWorkerFp("ab".repeat(32)),
    sink: new SessionEventTestSink(),
    sendBinaryUpstream: send,
  });
  managers.add(manager);
  return manager;
}

function addChannel(manager: SessionManager, channelId: number): void {
  manager.sessions.set(channelId, {} as never);
}

function enqueue(
  manager: SessionManager,
  channelId: number,
  endSeq: number,
  text: string | Buffer,
): void {
  manager._enqueueRawMetadata(
    channelId,
    endSeq,
    typeof text === "string" ? Buffer.from(text) : text,
  );
}

async function flushRawMetadata(): Promise<void> {
  await new Promise<void>((resolve) => queueMicrotask(resolve));
}

afterEach(async () => {
  for (const manager of managers) {
    for (const channelId of manager.sessions.keys()) {
      disposeRawMetadataState(manager, channelId);
    }
    for (const channelId of [...manager.rawMetadataQueues.keys()]) {
      disposeRawMetadataState(manager, channelId);
    }
    manager.sessions.clear();
  }
  managers.clear();
  await flushRawMetadata();
  vi.useRealTimers();
});

describe("worker raw metadata dispatcher", () => {
  test("round-robins copied FIFO heads and treats queued as accepted", async () => {
    const deliveries: Array<{
      channelId: number;
      direction: number;
      endSeq: number;
      text: string;
    }> = [];
    const manager = makeManager((channelId, direction, endSeq, bytes) => {
      deliveries.push({
        channelId,
        direction,
        endSeq,
        text: Buffer.from(bytes).toString("utf8"),
      });
      return endSeq === 12 ? "queued" : "sent";
    });
    addChannel(manager, 11);
    addChannel(manager, 22);

    const copiedSource = Buffer.from("first");
    enqueue(manager, 11, 11, copiedSource);
    copiedSource[0] = 0x58;
    enqueue(manager, 11, 12, "second");
    enqueue(manager, 22, 21, "third");
    enqueue(manager, 22, 22, "fourth");

    expect(manager.rawMetadataReadyRing).toEqual(new Set([11, 22]));
    await flushRawMetadata();

    expect(deliveries).toEqual([
      { channelId: 11, direction: DIR_FROM_PTY, endSeq: 11, text: "first" },
      { channelId: 22, direction: DIR_FROM_PTY, endSeq: 21, text: "third" },
      { channelId: 11, direction: DIR_FROM_PTY, endSeq: 12, text: "second" },
      { channelId: 22, direction: DIR_FROM_PTY, endSeq: 22, text: "fourth" },
    ]);
    expect(manager.rawMetadataQueuedBytes).toBe(0);
    expect(manager.rawMetadataQueues.size).toBe(0);
    expect(manager.rawMetadataReadyRing).toEqual(new Set());
  });

  test("uses one bounded global delayed wake for a ready backlog", async () => {
    vi.useFakeTimers();
    let attempts = 0;
    const manager = makeManager(() => {
      attempts += 1;
      return "sent";
    });
    const channelCount = RAW_METADATA_DISPATCH_FRAME_BUDGET + 1;
    for (let channelId = 1; channelId <= channelCount; channelId += 1) {
      addChannel(manager, channelId);
      enqueue(manager, channelId, channelId, "frame");
    }

    await flushRawMetadata();

    expect(attempts).toBeGreaterThan(0);
    expect(attempts).toBeLessThanOrEqual(RAW_METADATA_DISPATCH_FRAME_BUDGET);
    expect(manager.rawMetadataReadyRing.size).toBe(channelCount - attempts);
    const wake = manager.rawMetadataWake;
    expect(wake).toMatchObject({ kind: "timer" });

    const lateChannel = channelCount + 1;
    const attemptsBeforeLateFrame = attempts;
    addChannel(manager, lateChannel);
    enqueue(manager, lateChannel, lateChannel, "late");
    await flushRawMetadata();

    expect(attempts).toBe(attemptsBeforeLateFrame);
    expect(manager.rawMetadataWake).toBe(wake);
    expect(manager.rawMetadataReadyRing.has(lateChannel)).toBe(true);
  });

  test("drains a late frame when the shared trailing wake fires", async () => {
    vi.useFakeTimers();
    const deliveries: number[] = [];
    const manager = makeManager((_channelId, _direction, endSeq) => {
      deliveries.push(endSeq);
      return "sent";
    });
    addChannel(manager, 35);
    enqueue(manager, 35, 1, "first");
    await flushRawMetadata();
    expect(manager.rawMetadataWake).toMatchObject({ kind: "timer" });

    addChannel(manager, 36);
    enqueue(manager, 36, 2, "late");
    await flushRawMetadata();
    expect(deliveries).toEqual([1]);

    vi.advanceTimersByTime(CELL_EMIT_COALESCE_MS);
    await flushRawMetadata();

    expect(deliveries).toEqual([1, 2]);
    expect(manager.rawMetadataReadyRing).toEqual(new Set());
  });

  test("keeps per-channel and aggregate staging charges exact at their caps", () => {
    const manager = makeManager(() => "sent");
    const channelCapFrame = Buffer.alloc(RAW_METADATA_CHANNEL_CAP_BYTES, 0x61);
    const channelCount = RAW_METADATA_AGGREGATE_CAP_BYTES / RAW_METADATA_CHANNEL_CAP_BYTES;

    for (let channelId = 1; channelId <= channelCount; channelId += 1) {
      addChannel(manager, channelId);
      enqueue(manager, channelId, channelId, channelCapFrame);
    }
    enqueue(manager, 1, 99, "overflow");
    const overflowChannel = channelCount + 1;
    addChannel(manager, overflowChannel);
    enqueue(manager, overflowChannel, overflowChannel, "overflow");

    expect(manager.rawMetadataQueues.get(1)?.bytes).toBe(RAW_METADATA_CHANNEL_CAP_BYTES);
    expect(manager.rawMetadataQueuedBytes).toBe(RAW_METADATA_AGGREGATE_CAP_BYTES);
    expect(manager.rawMetadataQueues.has(overflowChannel)).toBe(false);
    expect(manager.rawMetadataReadyRing.size).toBe(channelCount);
  });

  test("drops only the rejected channel staging while ready peers continue", async () => {
    const deliveries: Array<[number, number]> = [];
    let manager: SessionManager;
    manager = makeManager((channelId, _direction, endSeq) => {
      deliveries.push([channelId, endSeq]);
      if (channelId === 31 && endSeq === 1) {
        enqueue(manager, channelId, 5, "drop-reentrant");
      }
      return channelId === 31 ? "dropped" : "sent";
    });
    addChannel(manager, 31);
    addChannel(manager, 32);
    enqueue(manager, 31, 1, "drop-one");
    enqueue(manager, 31, 2, "drop-two");
    enqueue(manager, 32, 3, "keep-one");
    enqueue(manager, 32, 4, "keep-two");

    await flushRawMetadata();

    expect(deliveries).toEqual([
      [31, 1],
      [32, 3],
      [32, 4],
    ]);
    expect(manager.rawMetadataQueues.size).toBe(0);
    expect(manager.rawMetadataQueuedBytes).toBe(0);
    expect(manager.rawMetadataReadyRing).toEqual(new Set());
  });

  test("removes a queued channel token without canceling its peer wake", async () => {
    const deliveries: Array<[number, number]> = [];
    const manager = makeManager((channelId, _direction, endSeq) => {
      deliveries.push([channelId, endSeq]);
      return "sent";
    });
    addChannel(manager, 41);
    addChannel(manager, 42);
    enqueue(manager, 41, 1, "discarded");
    enqueue(manager, 41, 2, "discarded-too");
    enqueue(manager, 42, 3, "retained");
    const wake = manager.rawMetadataWake;

    manager._disposeOutputState(41);

    expect(manager.rawMetadataReadyRing).toEqual(new Set([42]));
    expect(manager.rawMetadataQueuedBytes).toBe(Buffer.byteLength("retained"));
    expect(manager.rawMetadataWake).toBe(wake);
    await flushRawMetadata();

    expect(deliveries).toEqual([[42, 3]]);
    expect(manager.rawMetadataQueuedBytes).toBe(0);
    expect(manager.rawMetadataQueues.size).toBe(0);
  });

  test("does not double-charge a queue disposed from inside its send callback", async () => {
    const deliveries: number[] = [];
    let manager: SessionManager;
    manager = makeManager((channelId, _direction, endSeq) => {
      deliveries.push(endSeq);
      if (channelId === 51) manager._disposeOutputState(channelId);
      return "sent";
    });
    addChannel(manager, 51);
    addChannel(manager, 52);
    enqueue(manager, 51, 1, "closing");
    enqueue(manager, 52, 2, "surviving");

    await flushRawMetadata();

    expect(deliveries).toEqual([1, 2]);
    expect(manager.rawMetadataQueues.size).toBe(0);
    expect(manager.rawMetadataQueuedBytes).toBe(0);
    expect(manager.rawMetadataReadyRing).toEqual(new Set());
  });
});
