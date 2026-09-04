// Announcement barrier: an `opened`/`respawned` event is recognized
// synchronously, but its durable append is still queued. Until that append
// commits and binds (workerFp, channelId) → session, BOTH fan-out lanes buffer
// here — cell grids and raw PTY binary — and drain in their exact arrival order
// on the socket lane. A respawn's first binary frame can carry the only copy of
// the new PTY's title/OSC mapping, so it may not be reordered behind (or ahead
// of) the first cell frames.

import { afterEach, beforeEach, expect, test, vi } from "bun:test";
import { create } from "@bufbuild/protobuf";
import { PbCellGridFrameSchema } from "@roost/shared/proto/cell_pb";
import {
  CoordWorkerUpSchema,
  WBinarySchema,
  WCellGridSchema,
  type CoordWorkerUp,
} from "@roost/shared/proto/worker_transport_pb";
import {
  ANNOUNCED_CHANNEL_MAX_BYTES,
  ANNOUNCED_CHANNEL_MAX_FRAMES,
  ANNOUNCED_CHANNEL_MAX_MS,
  AnnouncedChannelBarrier,
  type AnnouncedDrop,
} from "../src/connect/announced-channel-barrier.ts";
import { WorkerRetainedWorkBudget } from "../src/connect/worker-frame-queue.ts";

const SESSION = "00000000-0000-4000-8000-000000000717";

function cell(seq: number, full: boolean): CoordWorkerUp {
  return create(CoordWorkerUpSchema, {
    frame: {
      case: "cellGrid",
      value: create(WCellGridSchema, {
        channelId: 7,
        frame: create(PbCellGridFrameSchema, {
          sessionId: SESSION,
          seq: BigInt(seq),
          full,
          cols: 80,
          rows: 24,
          gridEpoch: "announced",
        }),
      }),
    },
  });
}

function binary(seq: number, text: string): CoordWorkerUp {
  return create(CoordWorkerUpSchema, {
    frame: {
      case: "binary",
      value: create(WBinarySchema, {
        channelId: 7,
        direction: 1,
        seq: BigInt(seq),
        data: new TextEncoder().encode(text),
      }),
    },
  });
}

function label(frame: CoordWorkerUp): string {
  if (frame.frame.case === "cellGrid") {
    return `cell:${frame.frame.value.frame?.seq}:${frame.frame.value.frame?.full ? "full" : "delta"}`;
  }
  if (frame.frame.case === "binary") {
    return `binary:${new TextDecoder().decode(frame.frame.value.data)}`;
  }
  return `other:${frame.frame.case}`;
}

let drops: AnnouncedDrop[] = [];
let barrier: AnnouncedChannelBarrier;

beforeEach(() => {
  drops = [];
  const budget = new WorkerRetainedWorkBudget(() => {
    throw new Error("unexpected socket-wide overflow");
  });
  barrier = new AnnouncedChannelBarrier((drop) => { drops.push(drop); }, budget);
});

afterEach(() => {
  barrier.clear();
  vi.useRealTimers();
});

test("a respawn's metadata binary frame and its cell frames drain in arrival order", async () => {
  expect(barrier.enqueue(7, cell(1, true), 100)).toBe("not-announced");
  barrier.announce(7, SESSION);
  // The new PTY's title/OSC-8 write reaches coord BEFORE the new core's first
  // full grid; both must publish after the durable commit, in this order.
  expect(barrier.enqueue(7, binary(1, "\u001b]0;fresh-title\u0007"), 60)).toBe("buffered");
  expect(barrier.enqueue(7, cell(10, true), 100)).toBe("buffered");
  expect(barrier.enqueue(7, binary(2, "prompt$ "), 40)).toBe("buffered");
  expect(barrier.enqueue(7, cell(11, false), 100)).toBe("buffered");
  expect(barrier.stats()).toEqual({ channels: 1, frames: 4, bytes: 300, pending: 1, draining: 0 });

  const events: string[] = [];
  const committed = await barrier.commit(7, SESSION, () => true, async (frame) => {
    events.push(`start ${label(frame)}`);
    await Promise.resolve();
    events.push(`end ${label(frame)}`);
  });

  expect(committed).toBe(true);
  // Each frame is awaited before the next begins: no interleaving, no reorder.
  expect(events).toEqual([
    "start binary:\u001b]0;fresh-title\u0007",
    "end binary:\u001b]0;fresh-title\u0007",
    "start cell:10:full",
    "end cell:10:full",
    "start binary:prompt$ ",
    "end binary:prompt$ ",
    "start cell:11:delta",
    "end cell:11:delta",
  ]);
  expect(barrier.stats().channels).toBe(0);
  expect(drops).toEqual([]);
});

test("frames arriving during the drain join the tail instead of overtaking it", async () => {
  barrier.announce(7, SESSION);
  barrier.enqueue(7, cell(10, true), 100);
  barrier.enqueue(7, binary(1, "first"), 40);

  const delivered: string[] = [];
  const arrivals = [binary(2, "mid-drain"), cell(11, false)];
  const committed = await barrier.commit(7, SESSION, () => true, async (frame) => {
    delivered.push(label(frame));
    const next = arrivals.shift();
    // A frame that lands while the channel is draining still enters the buffer,
    // so the drain — not the fast path — publishes it.
    if (next) expect(barrier.enqueue(7, next, 40)).toBe("buffered");
    await Promise.resolve();
  });

  expect(committed).toBe(true);
  expect(delivered).toEqual(["cell:10:full", "binary:first", "binary:mid-drain", "cell:11:delta"]);
  // Buffer empty → channel open; the next arrival takes the fast path.
  expect(barrier.stats().channels).toBe(0);
  expect(barrier.enqueue(7, cell(12, false), 40)).toBe("not-announced");
});

test("frame-count overflow reports the pending loss of both lanes", () => {
  barrier.announce(7, SESSION);
  expect(barrier.enqueue(7, cell(10, true), 100)).toBe("buffered");
  for (let seq = 1; seq <= ANNOUNCED_CHANNEL_MAX_FRAMES - 1; seq++) {
    expect(barrier.enqueue(7, binary(seq, "ab"), 10)).toBe("buffered");
  }
  expect(barrier.enqueue(7, cell(11, false), 10)).toBe("dropped");

  expect(barrier.stats().channels).toBe(0);
  expect(drops).toHaveLength(1);
  const drop = drops[0]!;
  expect(drop.reason).toBe("overflow");
  expect(drop.phase).toBe("pending");
  expect(drop.channelId).toBe(7);
  expect(drop.sessionId).toBe(SESSION);
  // The rejected frame is lost too, so it is counted with the buffer.
  expect(drop.cellFrames).toBe(2);
  expect(drop.binaryFrames).toBe(ANNOUNCED_CHANNEL_MAX_FRAMES - 1);
  expect(drop.binaryBytes).toBe((ANNOUNCED_CHANNEL_MAX_FRAMES - 1) * 2);
});

test("byte-cap overflow drops the buffer and reports the dropped PTY bytes", () => {
  barrier.announce(7, SESSION);
  barrier.enqueue(7, cell(10, true), 100);
  barrier.enqueue(7, binary(1, "title-bytes"), 64);
  expect(barrier.enqueue(7, binary(2, "flood"), ANNOUNCED_CHANNEL_MAX_BYTES)).toBe("dropped");

  expect(drops).toHaveLength(1);
  expect(drops[0]!.reason).toBe("overflow");
  expect(drops[0]!.cellFrames).toBe(1);
  expect(drops[0]!.binaryFrames).toBe(2);
  expect(drops[0]!.binaryBytes).toBe("title-bytes".length + "flood".length);
});

test("a durable append that never commits times out and reports its loss", () => {
  vi.useFakeTimers();
  barrier.announce(7, SESSION);
  barrier.enqueue(7, cell(10, true), 100);
  barrier.enqueue(7, binary(1, "osc8-link"), 32);

  vi.advanceTimersByTime(ANNOUNCED_CHANNEL_MAX_MS - 1);
  expect(drops).toEqual([]);
  vi.advanceTimersByTime(1);

  expect(drops).toHaveLength(1);
  expect(drops[0]!.reason).toBe("timeout");
  expect(drops[0]!.phase).toBe("pending");
  expect(drops[0]!.cellFrames).toBe(1);
  expect(drops[0]!.binaryFrames).toBe(1);
  expect(drops[0]!.binaryBytes).toBe("osc8-link".length);
  expect(barrier.stats().channels).toBe(0);
});

test("a delta before the channel's first full grid is an ordering loss", () => {
  barrier.announce(7, SESSION);
  // A binary frame first is legitimate; a cell DELTA first is not.
  expect(barrier.enqueue(7, binary(1, "bytes"), 20)).toBe("buffered");
  expect(barrier.enqueue(7, cell(2, false), 100)).toBe("dropped");
  expect(drops).toHaveLength(1);
  expect(drops[0]!.reason).toBe("out_of_order");
  expect(drops[0]!.cellFrames).toBe(1);
  expect(drops[0]!.binaryFrames).toBe(1);

  barrier.announce(7, SESSION);
  expect(barrier.enqueue(7, cell(10, true), 100)).toBe("buffered");
  expect(barrier.enqueue(7, cell(12, false), 100)).toBe("dropped");
  expect(drops[1]!.reason).toBe("out_of_order");
});

test("commit without the exact binding delivers nothing", async () => {
  barrier.announce(7, SESSION);
  barrier.enqueue(7, cell(10, true), 100);
  barrier.enqueue(7, binary(1, "bytes"), 20);
  let delivered = 0;

  const committed = await barrier.commit(7, SESSION, () => false, async () => { delivered += 1; });

  expect(committed).toBe(false);
  expect(delivered).toBe(0);
  expect(drops).toHaveLength(1);
  expect(drops[0]!.reason).toBe("mapping_mismatch");
  expect(barrier.stats().channels).toBe(0);

  // A commit for another session never touches this channel's buffer.
  barrier.announce(7, SESSION);
  barrier.enqueue(7, cell(10, true), 100);
  expect(await barrier.commit(7, "other-session", () => true, async () => { delivered += 1; })).toBe(false);
  expect(delivered).toBe(0);
  expect(barrier.stats().channels).toBe(1);
});

test("a replacement announcement reports the buffer it can never bind", () => {
  barrier.announce(7, SESSION);
  barrier.enqueue(7, cell(10, true), 100);
  barrier.announce(7, SESSION);

  expect(drops).toHaveLength(1);
  expect(drops[0]!.reason).toBe("superseded");
  expect(drops[0]!.cellFrames).toBe(1);
  expect(barrier.stats()).toEqual({ channels: 1, frames: 0, bytes: 0, pending: 1, draining: 0 });
});
