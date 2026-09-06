// The coordinator's single write-availability switch. Every durable mutation
// takes a lease; a move flips the mode through draining→retired (source) or
// target_pending→active (target). setMode is the one choke point that sees
// every flip, so it owns the audit trail — highest-stakes state in the
// process, silent flips made move failures undiagnosable.

import { Code, ConnectError } from "@connectrpc/connect";
import { log } from "@roost/shared/log";

export type CoordinatorWriteMode = "active" | "source_draining" | "target_pending" | "retired";

export interface WriteLease {
  release(): void;
}

export class CoordinatorWriteGate {
  #mode: CoordinatorWriteMode;
  #leases = 0;
  #drained: (() => void)[] = [];
  #exclusiveOwner: string | null = null;
  #exclusivePhase: "draining" | "held" | null = null;
  #exclusiveReleased: (() => void)[] = [];

  constructor(mode: CoordinatorWriteMode = "active") {
    this.#mode = mode;
  }

  get mode(): CoordinatorWriteMode {
    return this.#mode;
  }

  get exclusive(): boolean {
    return this.#exclusiveOwner !== null;
  }

  get exclusiveHeld(): boolean {
    return this.#exclusivePhase === "held";
  }

  acquire(): WriteLease {
    if (this.#mode !== "active" || this.#exclusiveOwner !== null) {
      throw new ConnectError(
        this.#exclusiveOwner !== null
          ? "coordinator keeper update preparation in progress"
          : this.#mode === "source_draining"
            ? "coordinator move in progress"
            : "coordinator is not write-active",
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
    if (this.#mode !== "active" || this.#exclusivePhase === "held") {
      throw new ConnectError(
        this.#mode === "source_draining"
          ? "coordinator move in progress"
          : this.#exclusivePhase === "held"
            ? "coordinator keeper update preparation is held"
            : "coordinator is not write-active",
        Code.Unavailable,
      );
    }
    return this.#addLease();
  }

  async acquireAfterExclusive(timeoutMs = 30_000): Promise<WriteLease> {
    const deadline = Date.now() + timeoutMs;
    for (;;) {
      if (this.#mode !== "active") {
        throw new ConnectError("coordinator is not write-active", Code.Unavailable);
      }
      if (this.#exclusiveOwner === null) return this.#addLease();
      await this.#waitForExclusiveRelease(Math.max(1, deadline - Date.now()));
    }
  }

  async acquireExclusive(owner: string, timeoutMs = 30_000): Promise<WriteLease> {
    if (!owner) throw new Error("exclusive coordinator write owner is required");
    const deadline = Date.now() + timeoutMs;
    if (this.#mode !== "active") {
      throw new ConnectError("coordinator is not write-active", Code.Unavailable);
    }
    while (this.#exclusiveOwner !== null) {
      await this.#waitForExclusiveRelease(Math.max(1, deadline - Date.now()));
      if (this.#mode !== "active") {
        throw new ConnectError("coordinator is not write-active", Code.Unavailable);
      }
    }
    this.#exclusiveOwner = owner;
    this.#exclusivePhase = "draining";
    try {
      while (this.#leases !== 0) {
        await this.#waitForLeases(Math.max(1, deadline - Date.now()));
      }
      if (this.#mode !== "active") {
        throw new ConnectError("coordinator is not write-active", Code.Unavailable);
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

  async beginDrain(timeoutMs = 30_000): Promise<void> {
    if (this.#mode === "retired") return;
    const deadline = Date.now() + timeoutMs;
    while (this.#exclusiveOwner !== null) {
      await this.#waitForExclusiveRelease(Math.max(1, deadline - Date.now()));
    }
    this.#mode = "source_draining";
    await this.#waitForLeases(Math.max(1, deadline - Date.now()));
  }

  /** Single choke point for every gate transition — logs from→to so a stuck
   *  cluster's RPC rejections can be traced back to the flip that caused
   *  them. */
  setMode(mode: CoordinatorWriteMode): void {
    if (this.#exclusiveOwner !== null) {
      throw new ConnectError(
        "coordinator keeper update preparation in progress",
        Code.Unavailable,
      );
    }
    if (mode !== this.#mode) {
      log.info("coord-move", "gate_mode", { from: this.#mode, to: mode });
    }
    this.#mode = mode;
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
