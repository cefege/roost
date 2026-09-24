// Covers Sync-v2 terminal recipient ownership, credit admission, and timing stamps.
// The deterministic scheduler harness exposes actual sent bytes and ACK state.
// These tests keep terminal row/span payloads immutable while egress owns scalars.

import { expect, test } from "bun:test";
import { create } from "@bufbuild/protobuf";
import {
  PbCellGridChunkSchema,
  PbCellGridFrameSchema,
  PbCellRowSchema,
  PbCellSpanSchema,
} from "@roost/protocol/proto/cell_pb";
import {
  FirehoseFrameSchema,
  SyncDomain,
  type FirehoseFrame,
} from "@roost/protocol/proto/sync_pb";
import { ownV2ApplicationFrame } from "../../src/sync/sync-ws-v2-state.ts";
import { APPLICATION_MAX_UNACKED_FRAMES } from "../../src/sync/sync-ws-v1-delivery.ts";
import {
  TARGET_SESSION,
  decodedFrames,
  fillApplicationAckWindow,
  flushMicrotasks,
  makeChunk,
  makeHarness,
  snapshotSource,
  type SchedulerHarness,
} from "./sync-ws-v2-scheduler-harness.ts";

function makeStampedCell(coordRecvMs = 880n): FirehoseFrame {
  return create(FirehoseFrameSchema, {
    frame: {
      case: "cellGrid",
      value: create(PbCellGridFrameSchema, {
        sessionId: TARGET_SESSION,
        streamId: "egress-stream",
        gridEpoch: "egress-grid",
        cols: 80,
        rows: 24,
        seq: 7n,
        full: false,
        baseSeq: 6n,
        coordRecvMs,
        viewportRows: [create(PbCellRowSchema, {
          index: 3,
          spans: [create(PbCellSpanSchema, { text: "shared", columns: 6 })],
        })],
      }),
    },
  });
}

function startAnnouncedTerminal(harness: SchedulerHarness, streamId: string): void {
  harness.scheduler.beginTerminalStream(harness.ws, TARGET_SESSION, streamId);
  harness.socket.data.v2!.announcedSessions.add(TARGET_SESSION);
}

test("terminal ownership uses recipient shells while rows and spans stay shared", () => {
  const source = makeStampedCell();
  if (source.frame.case !== "cellGrid") throw new Error("expected cell source");
  const sourceCell = source.frame.value;
  const sourceRow = sourceCell.viewportRows[0]!;
  const sourceSpan = sourceRow.spans[0]!;

  const owned = ownV2ApplicationFrame(source, SyncDomain.TERMINAL, 41n).frame;
  if (owned.frame.case !== "cellGrid") throw new Error("expected owned cell");
  expect(owned).not.toBe(source);
  expect(owned.frame).not.toBe(source.frame);
  expect(owned.frame.value).not.toBe(sourceCell);
  expect(owned.frame.value.viewportRows).toBe(sourceCell.viewportRows);
  expect(owned.frame.value.viewportRows[0]).toBe(sourceRow);
  expect(owned.frame.value.viewportRows[0]!.spans).toBe(sourceRow.spans);
  expect(owned.frame.value.viewportRows[0]!.spans[0]).toBe(sourceSpan);

  const chunkSource = makeStampedCell();
  if (chunkSource.frame.case !== "cellGrid") throw new Error("expected chunk cell source");
  const chunk = create(FirehoseFrameSchema, {
    frame: {
      case: "cellGridChunk",
      value: create(PbCellGridChunkSchema, {
        snapshotId: "egress-snapshot",
        chunkIndex: 0,
        chunkCount: 2,
        part: chunkSource.frame.value,
      }),
    },
  });
  const ownedChunk = ownV2ApplicationFrame(chunk, SyncDomain.TERMINAL, 42n).frame;
  if (
    chunk.frame.case !== "cellGridChunk"
    || !chunk.frame.value.part
    || ownedChunk.frame.case !== "cellGridChunk"
    || !ownedChunk.frame.value.part
  ) throw new Error("expected chunk part");
  expect(ownedChunk.frame.value).not.toBe(chunk.frame.value);
  expect(ownedChunk.frame.value.part).not.toBe(chunk.frame.value.part);
  expect(ownedChunk.frame.value.part.viewportRows).toBe(chunk.frame.value.part.viewportRows);
  expect(ownedChunk.frame.value.part.viewportRows[0]).toBe(chunk.frame.value.part.viewportRows[0]);
  expect(ownedChunk.frame.value.part.viewportRows[0]!.spans)
    .toBe(chunk.frame.value.part.viewportRows[0]!.spans);
});

test("terminal egress waits for conservative ACK credit and charges exact sent bytes", async () => {
  const harness = makeHarness("scheduler-test:terminal-credit", true);
  const streamId = "stream-terminal-credit";
  startAnnouncedTerminal(harness, streamId);
  fillApplicationAckWindow(harness);

  const source = makeStampedCell();
  expect(harness.scheduler.enqueueTerminalDelta(
    harness.ws,
    TARGET_SESSION,
    streamId,
    source,
  )).toBe(true);
  await flushMicrotasks();

  const blocked = harness.terminal.queue[0]!;
  expect(harness.socket.encodedApplicationFrames).toBe(0);
  expect(harness.socket.sent).toEqual([]);
  harness.scheduler.scheduleV2(harness.ws);
  await flushMicrotasks();
  expect(harness.socket.encodedApplicationFrames).toBe(0);

  harness.clock.advance(23);
  expect(harness.delivery.applyCumulativeAck(
    harness.ws,
    harness.socket.data.lastSentDeliverySeq,
  )).toBe(true);
  await flushMicrotasks();

  const encodedBytes = harness.socket.sent[0]!.byteLength;
  expect(harness.socket.encodedApplicationFrames).toBe(1);
  const delivered = decodedFrames(harness.socket)[0]!;
  if (delivered.frame.case !== "cellGrid") throw new Error("expected delivered terminal cell");
  expect(delivered.frame.value.coordFanoutMs).toBe(1_023n);
  expect(blocked.estimatedBytes).toBeGreaterThanOrEqual(encodedBytes);
  expect(harness.socket.data.deliveryQueue).toHaveLength(1);
  expect(harness.socket.data.deliveryQueue[0]!.encodedBytes).toBe(encodedBytes);
  expect(harness.socket.data.unackedEncodedBytes).toBe(encodedBytes);
  expect(harness.delivery.applyCumulativeAck(
    harness.ws,
    harness.socket.data.lastSentDeliverySeq,
  )).toBe(true);
  expect(harness.socket.data.deliveryQueue).toEqual([]);
  expect(harness.socket.data.unackedEncodedBytes).toBe(0);
});

test("terminal egress stamps each recipient cell without changing ingress timing", async () => {
  const first = makeHarness("scheduler-test:terminal-stamp-first", true);
  const second = makeHarness("scheduler-test:terminal-stamp-second", true);
  const streamId = "stream-terminal-stamp";
  startAnnouncedTerminal(first, streamId);
  startAnnouncedTerminal(second, streamId);
  first.clock.advance(3);
  second.clock.advance(19);

  const source = makeStampedCell(777n);
  expect(first.scheduler.enqueueTerminalDelta(first.ws, TARGET_SESSION, streamId, source)).toBe(true);
  expect(second.scheduler.enqueueTerminalDelta(second.ws, TARGET_SESSION, streamId, source)).toBe(true);
  await flushMicrotasks();

  const firstFrame = decodedFrames(first.socket)[0]!;
  const secondFrame = decodedFrames(second.socket)[0]!;
  if (firstFrame.frame.case !== "cellGrid" || secondFrame.frame.case !== "cellGrid") {
    throw new Error("expected stamped cells");
  }
  if (source.frame.case !== "cellGrid") throw new Error("expected ingress cell");
  expect(firstFrame.frame.value.coordRecvMs).toBe(777n);
  expect(secondFrame.frame.value.coordRecvMs).toBe(777n);
  expect(firstFrame.frame.value.coordFanoutMs).toBe(1_003n);
  expect(secondFrame.frame.value.coordFanoutMs).toBe(1_019n);
  expect(source.frame.value.coordFanoutMs).toBe(0n);
});

test("chunked snapshots retain identical recipient metadata across delayed parts", async () => {
  const harness = makeHarness("scheduler-test:chunk-stamp", true);
  const streamId = "stream-chunk-stamp";
  startAnnouncedTerminal(harness, streamId);
  const chunks = [makeChunk(TARGET_SESSION, 9, 0), makeChunk(TARGET_SESSION, 9, 1)];
  for (const frame of chunks) {
    if (frame.frame.case !== "cellGridChunk" || !frame.frame.value.part) {
      throw new Error("expected chunk source");
    }
    frame.frame.value.part.full = true;
    frame.frame.value.part.coordRecvMs = 700n;
  }
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
    streamId,
    snapshotSource(chunks),
  )).toBe(true);
  await flushMicrotasks();

  expect(harness.socket.sent).toHaveLength(1);
  harness.clock.advance(25);
  harness.socket.data.deliveryQueue.length = 0;
  harness.socket.data.unackedEncodedBytes = 0;
  harness.socket.data.deliveryTimer = null;
  harness.scheduler.scheduleV2(harness.ws);
  await flushMicrotasks();

  const sentChunks = decodedFrames(harness.socket);
  expect(sentChunks).toHaveLength(2);
  const parts = sentChunks.map((frame) => {
    if (frame.frame.case !== "cellGridChunk" || !frame.frame.value.part) {
      throw new Error("expected sent chunk");
    }
    return frame.frame.value.part;
  });
  expect(parts.map((part) => part.coordRecvMs)).toEqual([700n, 700n]);
  expect(parts.map((part) => part.coordFanoutMs)).toEqual([1_000n, 1_000n]);
  for (const source of chunks) {
    if (source.frame.case !== "cellGridChunk" || !source.frame.value.part) {
      throw new Error("expected unchanged chunk source");
    }
    expect(source.frame.value.part.coordFanoutMs).toBe(0n);
  }
});

test("a dropped terminal send closes without ACK accounting or a retry", async () => {
  const harness = makeHarness("scheduler-test:terminal-send-drop", true);
  const streamId = "stream-terminal-send-drop";
  startAnnouncedTerminal(harness, streamId);
  harness.socket.sendResult = 0;

  expect(harness.scheduler.enqueueTerminalDelta(
    harness.ws,
    TARGET_SESSION,
    streamId,
    makeStampedCell(),
  )).toBe(true);
  await flushMicrotasks();

  expect(harness.socket.encodedApplicationFrames).toBe(1);
  expect(harness.socket.sendCalls).toBe(1);
  expect(harness.droppedFrames).toEqual([{
    frame: "cellGrid",
    encodedBytes: expect.any(Number),
    bufferedBytes: 0,
  }]);
  expect(harness.socket.data.pressureClosing).toBe(true);
  expect(harness.socket.data.deliveryQueue).toEqual([]);
  expect(harness.socket.data.unackedEncodedBytes).toBe(0);
  await flushMicrotasks();
  expect(harness.socket.sendCalls).toBe(1);
});
