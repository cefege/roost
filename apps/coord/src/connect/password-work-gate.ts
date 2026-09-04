// Native password hashing is deliberately memory-hard, so unconstrained requests
// could exhaust the coordinator. This gate bounds running and queued Argon2 work
// before any caller can consume hashing resources or hold a database transaction.

import { Code, ConnectError } from "@connectrpc/connect";
import { NATIVE_PASSWORD_ARGON2ID } from "@roost/shared/native-credentials";

const MAX_RUNNING = 2;
const MAX_QUEUED = 4;

interface QueuedWork {
  start(): void;
}

/**
 * Per-coordinator scheduler for memory-hard password work. At most two Argon2
 * operations run at once and four more may wait; callers beyond that bounded
 * capacity fail before starting any work.
 */
export class PasswordWorkGate {
  private runningCount = 0;
  private readonly queue: QueuedWork[] = [];

  get running(): number {
    return this.runningCount;
  }

  get queued(): number {
    return this.queue.length;
  }

  run<T>(work: () => Promise<T>): Promise<T> {
    if (this.runningCount < MAX_RUNNING) return this.start(work);
    if (this.queue.length >= MAX_QUEUED) {
      return Promise.reject(new ConnectError(
        "password work capacity exceeded",
        Code.ResourceExhausted,
        new Headers({ "retry-after": "60" }),
      ));
    }

    return new Promise<T>((resolve, reject) => {
      this.queue.push({
        start: () => {
          void this.start(work).then(resolve, reject);
        },
      });
    });
  }

  hash(password: string): Promise<string> {
    return this.run(() => Bun.password.hash(password, NATIVE_PASSWORD_ARGON2ID));
  }

  verify(password: string, passwordHash: string): Promise<boolean> {
    return this.run(() => Bun.password.verify(password, passwordHash));
  }

  private start<T>(work: () => Promise<T>): Promise<T> {
    this.runningCount++;
    return Promise.resolve()
      .then(work)
      .finally(() => {
        this.runningCount--;
        this.queue.shift()?.start();
      });
  }
}
