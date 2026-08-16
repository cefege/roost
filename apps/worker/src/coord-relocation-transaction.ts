import { acquireMachineTransaction } from "@roost/shared/machine-transaction";

export type RelocationTransactionOwner = "coordinator-check" | "coordinator-target" | "worker-endpoint";

interface MachineTransactionLease {
  release(): Promise<void> | void;
}

/**
 * One Windows worker participates in a coordinator move twice: CoordTarget owns
 * the promoted coordinator files, while WorkerCoordRelocation owns the worker
 * service endpoint. They must share one OS lock rather than deadlocking by
 * acquiring the named relocation lock independently.
 */
export class MachineRelocationTransaction {
  #handoffId: string | null = null;
  #lease: MachineTransactionLease | null = null;
  readonly #owners = new Set<RelocationTransactionOwner>();

  async acquire(
    platform: string,
    handoffId: string,
    journalPath: string,
    owner: RelocationTransactionOwner,
  ): Promise<void> {
    switch (platform) {
      case "darwin":
      case "linux":
        return;
      case "win32":
        break;
      default:
        throw new Error(`unsupported coordinator relocation platform: ${platform}`);
    }

    if (this.#lease) {
      if (this.#handoffId !== handoffId) {
        throw new Error(`another coordinator relocation is active for handoff ${this.#handoffId}`);
      }
      this.#owners.add(owner);
      return;
    }

    const lease = await acquireMachineTransaction("relocation", journalPath, { platform: "win32" });
    this.#handoffId = handoffId;
    this.#lease = lease;
    this.#owners.add(owner);
  }

  async release(platform: string, handoffId: string, owner: RelocationTransactionOwner): Promise<void> {
    switch (platform) {
      case "darwin":
      case "linux":
        return;
      case "win32":
        break;
      default:
        throw new Error(`unsupported coordinator relocation platform: ${platform}`);
    }

    if (!this.#lease || this.#handoffId !== handoffId) return;
    this.#owners.delete(owner);
    if (this.#owners.size > 0) return;

    const lease = this.#lease;
    this.#lease = null;
    this.#handoffId = null;
    await lease.release();
  }
}

export const machineRelocationTransaction = new MachineRelocationTransaction();
