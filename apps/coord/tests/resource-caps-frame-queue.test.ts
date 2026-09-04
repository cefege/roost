/**
 * Covers the coordinator queue caps that bound pending worker frames and bytes.
 * Bun discovers this suite directly and drives the queue with controlled promises.
 * The cases depend only on the production ordered worker frame queue.
 */
import { describe, expect, test } from "bun:test";
import {
  OrderedWorkerFrameQueue,
  WORKER_FRAME_QUEUE_MAX_BYTES,
  WORKER_FRAME_QUEUE_MAX_FRAMES,
  type WorkerFrameQueueOverflow,
} from "../src/connect/worker-frame-queue.ts";

describe("ordered worker frame queue", () => {
  test("preserves order and accounts an in-flight frame until completion", async () => {
    const first = Promise.withResolvers<void>();
    const order: string[] = [];
    const queue = new OrderedWorkerFrameQueue<number>(
      async (value) => {
        order.push(`start:${value}`);
        if (value === 1) await first.promise;
        order.push(`end:${value}`);
      },
      (error) => { throw error; },
      () => { throw new Error("unexpected overflow"); },
    );
    expect(queue.enqueue(1, 7)).toBe("enqueued");
    expect(queue.enqueue(2, 11)).toBe("enqueued");
    expect(queue.stats()).toEqual({ frames: 2, bytes: 18 });
    expect(order).toEqual(["start:1"]);
    first.resolve();
    await queue.whenIdle();
    expect(order).toEqual(["start:1", "end:1", "start:2", "end:2"]);
    expect(queue.stats()).toEqual({ frames: 0, bytes: 0 });
  });

  test("allows 256 accounted frames and rejects the next before enqueue", async () => {
    const gate = Promise.withResolvers<void>();
    const processed: number[] = [];
    let overflow: WorkerFrameQueueOverflow | undefined;
    let closed = false;
    const queue = new OrderedWorkerFrameQueue<number>(
      async (value) => {
        processed.push(value);
        if (value === 0) await gate.promise;
      },
      (error) => { throw error; },
      (details) => {
        closed = true;
        overflow = details;
      },
    );
    for (let index = 0; index < WORKER_FRAME_QUEUE_MAX_FRAMES; index += 1) {
      expect(queue.enqueue(index, 1)).toBe("enqueued");
    }
    expect(queue.stats()).toEqual({
      frames: WORKER_FRAME_QUEUE_MAX_FRAMES,
      bytes: WORKER_FRAME_QUEUE_MAX_FRAMES,
    });
    expect(queue.enqueue(WORKER_FRAME_QUEUE_MAX_FRAMES, 1)).toBe("overflow");
    expect(closed).toBe(true);
    expect(overflow).toEqual({
      frames: WORKER_FRAME_QUEUE_MAX_FRAMES,
      bytes: WORKER_FRAME_QUEUE_MAX_FRAMES,
      rejectedBytes: 1,
    });
    expect(processed).toEqual([0]);
    gate.resolve();
    await queue.whenIdle();
    expect(processed).toEqual([0]);
  });

  test("allows exactly 16 MiB including in-flight bytes and rejects one more", async () => {
    const gate = Promise.withResolvers<void>();
    let overflow: WorkerFrameQueueOverflow | undefined;
    const queue = new OrderedWorkerFrameQueue<number>(
      async (value) => {
        if (value === 0) await gate.promise;
      },
      (error) => { throw error; },
      (details) => { overflow = details; },
    );
    const quarter = WORKER_FRAME_QUEUE_MAX_BYTES / 4;
    for (let index = 0; index < 4; index += 1) {
      expect(queue.enqueue(index, quarter)).toBe("enqueued");
    }
    expect(queue.stats()).toEqual({ frames: 4, bytes: WORKER_FRAME_QUEUE_MAX_BYTES });
    expect(queue.enqueue(4, 1)).toBe("overflow");
    expect(overflow).toEqual({
      frames: 4,
      bytes: WORKER_FRAME_QUEUE_MAX_BYTES,
      rejectedBytes: 1,
    });
    gate.resolve();
    await queue.whenIdle();
  });
});
