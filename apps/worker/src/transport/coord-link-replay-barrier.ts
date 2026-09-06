// Awaitable generation-aware barrier for CoordLink durable event replay.
// It resolves before snapshot activation, survives disconnects while pending,
// and rejects only caller cancellation or permanent link disposal.

interface ReplayWaiter {
  resolve(): void;
  reject(error: unknown): void;
  signal?: AbortSignal;
  abort?: () => void;
}

export class DurableSessionEventReplayBarrier {
  private readonly waiters = new Set<ReplayWaiter>();
  private drained = false;
  private disposed = false;

  wait(signal?: AbortSignal): Promise<void> {
    if (this.disposed) {
      return Promise.reject(new Error("coordinator link is disposed"));
    }
    if (signal?.aborted) return Promise.reject(abortError());
    if (this.drained) return Promise.resolve();
    const deferred = Promise.withResolvers<void>();
    const waiter: ReplayWaiter = {
      resolve: deferred.resolve,
      reject: deferred.reject,
      signal,
    };
    if (signal) {
      waiter.abort = () => {
        if (!this.waiters.delete(waiter)) return;
        deferred.reject(abortError());
      };
      signal.addEventListener("abort", waiter.abort, { once: true });
    }
    this.waiters.add(waiter);
    return deferred.promise;
  }

  markPending(): void {
    if (!this.disposed) this.drained = false;
  }

  markDrained(): void {
    if (this.disposed || this.drained) return;
    this.drained = true;
    for (const waiter of this.waiters) {
      removeAbortListener(waiter);
      waiter.resolve();
    }
    this.waiters.clear();
  }

  dispose(): void {
    if (this.disposed) return;
    this.disposed = true;
    this.drained = false;
    const error = new Error("coordinator link is disposed");
    for (const waiter of this.waiters) {
      removeAbortListener(waiter);
      waiter.reject(error);
    }
    this.waiters.clear();
  }
}

function removeAbortListener(waiter: ReplayWaiter): void {
  if (waiter.signal && waiter.abort) {
    waiter.signal.removeEventListener("abort", waiter.abort);
  }
}

function abortError(): Error {
  const error = new Error("durable session-event replay wait aborted");
  error.name = "AbortError";
  return error;
}
