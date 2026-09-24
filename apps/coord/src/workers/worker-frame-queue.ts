// Worker sockets share one retained-work budget across ordered control frames
// and frames waiting behind durable channel announcements. Every owner charges
// before retention and releases only after processing or delivery settles.

export const WORKER_FRAME_QUEUE_MAX_FRAMES = 256;
export const WORKER_FRAME_QUEUE_MAX_BYTES = 16 * 1024 * 1024;

export interface WorkerFrameQueueStats {
  /** Queue plus announcement-barrier frames retained by this socket. */
  frames: number;
  /** Queue plus announcement-barrier bytes retained by this socket. */
  bytes: number;
}

export interface WorkerFrameQueueOverflow extends WorkerFrameQueueStats {
  rejectedBytes: number;
}

export type WorkerFrameEnqueueResult = "enqueued" | "closed" | "overflow";

export type WorkerFrameRetainResult = "retained" | "closed" | "overflow";

/** Socket-wide accounting shared by every inbound frame retention owner. */
export class WorkerRetainedWorkBudget {
  private retainedFrames = 0;
  private retainedBytes = 0;
  private open = true;

  constructor(
    private readonly onOverflow: (overflow: WorkerFrameQueueOverflow) => void,
  ) {}

  retain(bytes: number): WorkerFrameRetainResult {
    if (!this.open) return "closed";
    if (
      !Number.isSafeInteger(bytes)
      || bytes < 0
      || this.retainedFrames >= WORKER_FRAME_QUEUE_MAX_FRAMES
      || bytes > WORKER_FRAME_QUEUE_MAX_BYTES - this.retainedBytes
    ) {
      const overflow = {
        frames: this.retainedFrames,
        bytes: this.retainedBytes,
        rejectedBytes: bytes,
      };
      this.open = false;
      this.onOverflow(overflow);
      return "overflow";
    }
    this.retainedFrames += 1;
    this.retainedBytes += bytes;
    return "retained";
  }

  release(bytes: number): void {
    if (
      !Number.isSafeInteger(bytes)
      || bytes < 0
      || this.retainedFrames === 0
      || bytes > this.retainedBytes
    ) {
      throw new Error("worker retained-work budget underflow");
    }
    this.retainedFrames -= 1;
    this.retainedBytes -= bytes;
  }

  close(): void {
    this.open = false;
  }

  isOpen(): boolean {
    return this.open;
  }

  stats(): WorkerFrameQueueStats {
    return { frames: this.retainedFrames, bytes: this.retainedBytes };
  }
}

interface AccountedFrame<T> {
  value: T;
  bytes: number;
}

/**
 * Per-socket ordered async work queue. Its budget remains charged while the
 * handler settles and is shared with the socket's announcement barrier.
 * Overflow latches every retention owner closed before accepting more work.
 */
export class OrderedWorkerFrameQueue<T> {
  private readonly waiting: Array<AccountedFrame<T>> = [];
  private draining = false;
  private readonly idleWaiters: Array<() => void> = [];
  readonly retainedWorkBudget: WorkerRetainedWorkBudget;

  constructor(
    private readonly process: (value: T) => Promise<void>,
    private readonly onFailure: (error: unknown, value: T) => void,
    onOverflow: (overflow: WorkerFrameQueueOverflow) => void,
  ) {
    this.retainedWorkBudget = new WorkerRetainedWorkBudget((overflow) => {
      this.close();
      onOverflow(overflow);
    });
  }

  enqueue(value: T, bytes: number): WorkerFrameEnqueueResult {
    const retained = this.retainedWorkBudget.retain(bytes);
    if (retained !== "retained") return retained;
    this.waiting.push({ value, bytes });
    if (!this.draining) void this.drain();
    return "enqueued";
  }

  close(): void {
    this.retainedWorkBudget.close();
    this.discardWaiting();
    this.resolveIdleWaiters();
  }

  isOpen(): boolean {
    return this.retainedWorkBudget.isOpen();
  }

  stats(): WorkerFrameQueueStats {
    return this.retainedWorkBudget.stats();
  }

  whenIdle(): Promise<void> {
    if (!this.draining && this.waiting.length === 0) return Promise.resolve();
    const { promise, resolve } = Promise.withResolvers<void>();
    this.idleWaiters.push(resolve);
    return promise;
  }

  private discardWaiting(): void {
    for (const item of this.waiting) {
      this.retainedWorkBudget.release(item.bytes);
    }
    this.waiting.length = 0;
  }

  private resolveIdleWaiters(): void {
    if (this.draining || this.waiting.length !== 0) return;
    for (const resolve of this.idleWaiters.splice(0)) resolve();
  }

  private async drain(): Promise<void> {
    this.draining = true;
    try {
      while (this.retainedWorkBudget.isOpen() && this.waiting.length > 0) {
        const item = this.waiting.shift()!;
        let failed = false;
        let failure: unknown;
        try {
          await this.process(item.value);
        } catch (error) {
          failed = true;
          failure = error;
        } finally {
          this.retainedWorkBudget.release(item.bytes);
        }
        if (failed) {
          const reportFailure = this.retainedWorkBudget.isOpen();
          this.retainedWorkBudget.close();
          this.discardWaiting();
          if (reportFailure) this.onFailure(failure, item.value);
          return;
        }
      }
    } finally {
      this.draining = false;
      this.resolveIdleWaiters();
    }
  }
}
