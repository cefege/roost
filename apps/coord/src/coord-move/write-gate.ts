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

  constructor(mode: CoordinatorWriteMode = "active") {
    this.#mode = mode;
  }

  get mode(): CoordinatorWriteMode {
    return this.#mode;
  }

  acquire(): WriteLease {
    if (this.#mode !== "active") {
      throw new ConnectError(
        this.#mode === "source_draining" ? "coordinator move in progress" : "coordinator is not write-active",
        Code.Unavailable,
      );
    }
    this.#leases++;
    let released = false;
    return {
      release: () => {
        if (released) return;
        released = true;
        this.#leases--;
        if (this.#leases === 0) this.#drained.splice(0).forEach((resolve) => resolve());
      },
    };
  }

  async beginDrain(timeoutMs = 30_000): Promise<void> {
    if (this.#mode === "retired") return;
    this.#mode = "source_draining";
    if (this.#leases === 0) return;
    await new Promise<void>((resolve, reject) => {
      const releaseWaiter = () => {
        clearTimeout(timeout);
        resolve();
      };
      const timeout = setTimeout(() => {
        const index = this.#drained.indexOf(releaseWaiter);
        if (index >= 0) this.#drained.splice(index, 1);
        reject(new Error("coordinator move drain timed out"));
      }, timeoutMs);
      this.#drained.push(releaseWaiter);
    });
  }

  /** Single choke point for every gate transition — logs from→to so a stuck
   *  cluster's RPC rejections can be traced back to the flip that caused
   *  them. */
  setMode(mode: CoordinatorWriteMode): void {
    if (mode !== this.#mode) {
      log.info("coord-move", "gate_mode", { from: this.#mode, to: mode });
    }
    this.#mode = mode;
  }
}
