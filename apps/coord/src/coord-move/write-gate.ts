import { Code, ConnectError } from "@connectrpc/connect";

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

  setMode(mode: CoordinatorWriteMode): void {
    this.#mode = mode;
  }
}
