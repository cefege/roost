// SSE helper for tRPC subscriptions over H3. The tRPC fetch adapter
// sends each yielded value as a Server-Sent Events data: frame.
// R0.2 — httpSubscriptionLink, NOT the ws adapter.
//
// Returns AsyncIterable<T> to match tRPC v11's subscription resolver
// signature: `resolver(opts) => MaybePromise<AsyncIterable<T>>`.

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
