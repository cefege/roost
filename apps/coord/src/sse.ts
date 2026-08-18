// Bus-to-AsyncIterable adapter. Turns a BoundedBus subscription into an
// AsyncIterable so a handler can `for await` over live bus messages and stop
// cleanly on an AbortSignal.
//
// Sole caller: deploy-jobs.ts streams a deploy job's log bus to the client
// through this. The name is historical — nothing here is Server-Sent Events.

import type { BoundedBus } from "./buses.ts";

// Converts a BoundedBus<T> subscription into an AsyncIterable<T>.
// Unsubscribes when the AbortSignal fires or the generator is abandoned.
export function busToAsyncIterable<T>(
  bus: BoundedBus<T>,
  opts: { signal?: AbortSignal } = {},
): AsyncIterable<T> {
  return {
    [Symbol.asyncIterator](): AsyncIterator<T> {
      const queue: T[] = [];
      let resolve: (() => void) | null = null;
      let done = false;

      const enqueue = (msg: T): void => {
        queue.push(msg);
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
            if (queue.length > 0) return { value: queue.shift()!, done: false };
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
