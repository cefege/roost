// The coordinator's write-exclusivity fence for keeper updates: every durable
// mutation takes a lease, and proving a keeper empty takes the exclusive lease,
// which drains admitted writes first and blocks new ones until released.
// Three orderings depend on it, and losing any of them loses live PTYs —
// worker-frame-dispatch withholds a durable event ACK while it is held,
// worker-respawn waits through it, terminal writes lease only inside the FIFO.

import { Code, ConnectError } from "@connectrpc/connect";
import { log } from "@roost/shared/log";

export interface WriteLease {
  release(): void;
}

export class CoordinatorWriteGate {
  #leases = 0;
  #drained: (() => void)[] = [];
  #exclusiveOwner: string | null = null;
  #exclusivePhase: "draining" | "held" | null = null;
  #exclusiveReleased: (() => void)[] = [];

  get exclusive(): boolean {
    return this.#exclusiveOwner !== null;
  }

  get exclusiveHeld(): boolean {
    return this.#exclusivePhase === "held";
  }

  acquire(): WriteLease {
    if (this.#exclusiveOwner !== null) {
      throw new ConnectError(
        "coordinator keeper update preparation in progress",
        Code.Unavailable,
      );
    }
    return this.#addLease();
  }

  /** Lifecycle projections may finish channel-creating commands admitted
   * before an exclusive update drain. They cannot create a keeper channel by
   * themselves, and allowing them is what lets the originating RPC lease
   * settle instead of deadlocking the drain. */
  acquireCompletion(): WriteLease {
    if (this.#exclusivePhase === "held") {
      throw new ConnectError(
        "coordinator keeper update preparation is held",
        Code.Unavailable,
      );
    }
    return this.#addLease();
  }

  async acquireAfterExclusive(timeoutMs = 30_000): Promise<WriteLease> {
    const deadline = Date.now() + timeoutMs;
    for (;;) {
      if (this.#exclusiveOwner === null) return this.#addLease();
      await this.#waitForExclusiveRelease(Math.max(1, deadline - Date.now()));
    }
  }

  async acquireExclusive(owner: string, timeoutMs = 30_000): Promise<WriteLease> {
    if (!owner) throw new Error("exclusive coordinator write owner is required");
    const deadline = Date.now() + timeoutMs;
    while (this.#exclusiveOwner !== null) {
      await this.#waitForExclusiveRelease(Math.max(1, deadline - Date.now()));
    }
    this.#exclusiveOwner = owner;
    this.#exclusivePhase = "draining";
    try {
      while (this.#leases !== 0) {
        await this.#waitForLeases(Math.max(1, deadline - Date.now()));
      }
      this.#exclusivePhase = "held";
      this.#leases += 1;
    } catch (error) {
      this.#exclusiveOwner = null;
      this.#exclusivePhase = null;
      this.#resolveExclusiveReleased();
      throw error;
    }
    log.info("coord-write-gate", "exclusive_acquired", { owner });
    let released = false;
    return {
      release: () => {
        if (released) return;
        released = true;
        this.#leases -= 1;
        this.#exclusiveOwner = null;
        this.#exclusivePhase = null;
        log.info("coord-write-gate", "exclusive_released", { owner });
        this.#resolveDrained();
        this.#resolveExclusiveReleased();
      },
    };
  }

  #addLease(): WriteLease {
    this.#leases += 1;
    let released = false;
    return {
      release: () => {
        if (released) return;
        released = true;
        this.#leases -= 1;
        this.#resolveDrained();
      },
    };
  }

  #resolveDrained(): void {
    if (this.#leases === 0) {
      this.#drained.splice(0).forEach((resolve) => resolve());
    }
  }

  async #waitForLeases(timeoutMs: number): Promise<void> {
    if (this.#leases === 0) return;
    await new Promise<void>((resolve, reject) => {
      const releaseWaiter = () => {
        clearTimeout(timeout);
        resolve();
      };
      const timeout = setTimeout(() => {
        const index = this.#drained.indexOf(releaseWaiter);
        if (index >= 0) this.#drained.splice(index, 1);
        reject(new Error("coordinator write drain timed out"));
      }, timeoutMs);
      this.#drained.push(releaseWaiter);
    });
  }

  #resolveExclusiveReleased(): void {
    this.#exclusiveReleased.splice(0).forEach((resolve) => resolve());
  }

  async #waitForExclusiveRelease(timeoutMs: number): Promise<void> {
    await new Promise<void>((resolve, reject) => {
      const releaseWaiter = () => {
        clearTimeout(timeout);
        resolve();
      };
      const timeout = setTimeout(() => {
        const index = this.#exclusiveReleased.indexOf(releaseWaiter);
        if (index >= 0) this.#exclusiveReleased.splice(index, 1);
        reject(new Error("coordinator keeper update queue timed out"));
      }, timeoutMs);
      this.#exclusiveReleased.push(releaseWaiter);
    });
  }
}
