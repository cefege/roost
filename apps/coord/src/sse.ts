// Bus-to-AsyncIterable adapter. Turns a BoundedBus subscription into an
// AsyncIterable so a handler can `for await` over live bus messages and stop
// cleanly on an AbortSignal.
//
// The per-subscriber queue is BOUNDED (frames + optional bytes, mirroring the
// sync-ws v1 application window): a stalled HTTP reader must exert the same
// close-for-backpressure semantics a WS would. On overflow the subscription
// is torn down, a Tier-1 signal fires, and next() rejects — callers decide
// whether to surface that as a stream error or a terminal frame.

import type { BoundedBus } from "./buses.ts";
import { signal } from "@roost/shared/diag";

/** Magnitudes mirror sync-ws-v1-delivery APPLICATION_MAX_UNACKED_*. */
const DEFAULT_MAX_FRAMES = 512;
const DEFAULT_MAX_BYTES = 4 * 1024 * 1024;

/** Thrown from next() when the per-subscriber bound is exceeded. */
export class SseQueueOverflowError extends Error {
  constructor(readonly frames: number, readonly bytes: number) {
    super(`sse subscriber queue overflowed (${frames} frames, ${bytes} bytes)`);
  }
}

// Converts a BoundedBus<T> subscription into an AsyncIterable<T>.
// Unsubscribes when the AbortSignal fires or the generator is abandoned.
export function busToAsyncIterable<T>(
  bus: BoundedBus<T>,
  opts: {
    signal?: AbortSignal;
    maxFrames?: number;
    maxBytes?: number;
    /** Byte-weight of one message; without it only the frame cap applies. */
    sizeOf?: (msg: T) => number;
  } = {},
): AsyncIterable<T> {
  return {
    [Symbol.asyncIterator](): AsyncIterator<T> {
      const queue: T[] = [];
      const maxFrames = opts.maxFrames ?? DEFAULT_MAX_FRAMES;
      const maxBytes = opts.maxBytes ?? DEFAULT_MAX_BYTES;
      let queuedBytes = 0;
      let resolve: (() => void) | null = null;
      let done = false;
      let overflow: SseQueueOverflowError | null = null;

      const enqueue = (msg: T): void => {
        if (done || overflow) return;
        queuedBytes += opts.sizeOf ? opts.sizeOf(msg) : 0;
        queue.push(msg);
        if (queue.length > maxFrames || queuedBytes > maxBytes) {
          overflow = new SseQueueOverflowError(queue.length, queuedBytes);
          cleanup();
          // SignalKind vocabulary is shared with the WS path; the sync
          // high-water kind is exactly this failure shape.
          signal("sync.queue_overflow", {
            frames: queue.length,
            bytes: queuedBytes,
            max_frames: maxFrames,
            max_bytes: maxBytes,
          });
          return;
        }
        if (resolve) { const r = resolve; resolve = null; r(); }
      };

      const unsub = bus.subscribe(enqueue);

      const cleanup = (): void => {
        if (done) return;
        done = true;
        unsub();
        if (resolve) { const r = resolve; resolve = null; r(); }
      };

      opts.signal?.addEventListener("abort", cleanup, { once: true });

      return {
        async next(): Promise<IteratorResult<T>> {
          while (true) {
            if (overflow) { const e = overflow; throw e; }
            if (queue.length > 0) {
              const value = queue.shift()!;
              queuedBytes -= opts.sizeOf ? opts.sizeOf(value) : 0;
              return { value, done: false };
            }
            if (done) return { value: undefined as unknown as T, done: true };
            await new Promise<void>((r) => { resolve = r; });
          }
        },
        async return(): Promise<IteratorResult<T>> {
          cleanup();
          opts.signal?.removeEventListener("abort", cleanup);
          return { value: undefined as unknown as T, done: true };
        },
      };
    },
  };
}
