// Owns server-wide serialization for dashboard-global search batches per worker.
// Session handlers share one instance so separate browsers cannot overlap a worker.
// Queue admission is bounded and every wait consumes the caller's page deadline.

export const _GLOBAL_SEARCH_MAX_WAITERS_PER_WORKER = 32;

interface GlobalSearchLaneWaiter {
  deadlineAtMs: number;
  signal: AbortSignal;
  timer: ReturnType<typeof setTimeout>;
  onAbort: () => void;
  resolve: (lease: GlobalSearchWorkerLease | null) => void;
}

interface GlobalSearchLaneState {
  activeToken: symbol | null;
  waiters: GlobalSearchLaneWaiter[];
}

export interface GlobalSearchWorkerLease {
  release(): void;
}

type SetGlobalSearchTimer = (
  callback: () => void,
  delayMs: number,
) => ReturnType<typeof setTimeout>;
type ClearGlobalSearchTimer = (timer: ReturnType<typeof setTimeout>) => void;

export interface GlobalSearchWorkerLaneOwnerOptions {
  now?: () => number;
  setTimer?: SetGlobalSearchTimer;
  clearTimer?: ClearGlobalSearchTimer;
}

/** Serializes global-search pages per worker while leaving distinct workers parallel. */
export class GlobalSearchWorkerLaneOwner {
  readonly #now: () => number;
  readonly #setTimer: SetGlobalSearchTimer;
  readonly #clearTimer: ClearGlobalSearchTimer;
  readonly #lanes = new Map<string, GlobalSearchLaneState>();

  constructor(options: GlobalSearchWorkerLaneOwnerOptions = {}) {
    this.#now = options.now ?? (() => performance.now());
    this.#setTimer = options.setTimer ?? setTimeout;
    this.#clearTimer = options.clearTimer ?? clearTimeout;
  }

  deadlineAfter(durationMs: number): number {
    return this.#now() + durationMs;
  }

  remainingMs(deadlineAtMs: number): number {
    return deadlineAtMs - this.#now();
  }

  acquire(
    workerFp: string,
    deadlineAtMs: number,
    signal: AbortSignal,
  ): Promise<GlobalSearchWorkerLease | null> {
    const remainingMs = this.remainingMs(deadlineAtMs);
    if (remainingMs <= 0 || signal.aborted) return Promise.resolve(null);
    const lane = this.#lanes.get(workerFp) ?? {
      activeToken: null,
      waiters: [],
    };
    this.#lanes.set(workerFp, lane);
    if (lane.activeToken === null) {
      return Promise.resolve(this.#grant(workerFp, lane));
    }
    if (lane.waiters.length >= _GLOBAL_SEARCH_MAX_WAITERS_PER_WORKER) {
      return Promise.resolve(null);
    }
    const { promise, resolve } =
      Promise.withResolvers<GlobalSearchWorkerLease | null>();
    const waiter = {} as GlobalSearchLaneWaiter;
    const retire = (): void => {
      const index = lane.waiters.indexOf(waiter);
      if (index < 0) return;
      lane.waiters.splice(index, 1);
      this.#clearTimer(waiter.timer);
      signal.removeEventListener("abort", waiter.onAbort);
      resolve(null);
    };
    waiter.deadlineAtMs = deadlineAtMs;
    waiter.signal = signal;
    waiter.resolve = resolve;
    waiter.onAbort = retire;
    waiter.timer = this.#setTimer(retire, remainingMs);
    waiter.timer.unref?.();
    signal.addEventListener("abort", retire, { once: true });
    lane.waiters.push(waiter);
    return promise;
  }

  #grant(
    workerFp: string,
    lane: GlobalSearchLaneState,
  ): GlobalSearchWorkerLease {
    const token = Symbol(workerFp);
    lane.activeToken = token;
    let released = false;
    return {
      release: (): void => {
        if (released || lane.activeToken !== token) return;
        released = true;
        while (lane.waiters.length > 0) {
          const waiter = lane.waiters.shift()!;
          this.#clearTimer(waiter.timer);
          waiter.signal.removeEventListener("abort", waiter.onAbort);
          if (
            waiter.signal.aborted
            || this.remainingMs(waiter.deadlineAtMs) <= 0
          ) {
            waiter.resolve(null);
            continue;
          }
          waiter.resolve(this.#grant(workerFp, lane));
          return;
        }
        lane.activeToken = null;
        if (this.#lanes.get(workerFp) === lane) this.#lanes.delete(workerFp);
      },
    };
  }
}
