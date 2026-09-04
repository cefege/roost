// Owns capacity admission and exposes the SaaS host configuration surface.
// Provisioning reserves accounts through this lease-guarded admission gate.
// Fresh backups, disk pressure, and account limits are checked inside the lease.
import { randomUUID } from "node:crypto";
import { existsSync, readdirSync, statSync } from "node:fs";
import { join } from "node:path";
import { SaasRegistryError, type SaasRegistry } from "./registry.ts";
import type { ProvisioningAdmission } from "./lifecycle.ts";
import {
  ADMISSION_LEASE_MS,
  ADMISSION_LEASE_RESOURCE,
  BACKUP_MAX_AGE_MS,
  type HostAdmissionOptions,
  type SaasHostConfig,
} from "./host-config.ts";
import { assertDisk, diskUsedRatio } from "./host-prerequisite-checks.ts";

export { loadSaasHostConfig } from "./host-config.ts";
export type { HostAdmissionOptions, SaasHostConfig } from "./host-config.ts";
export {
  assertSaasHostPrerequisites,
  assertSaasProvisionerStartupPrerequisites,
} from "./host-prerequisites.ts";

export class HostCapacityError extends Error {
  readonly code = "capacity";

  constructor() {
    super("managed account capacity has been reached");
    this.name = "HostCapacityError";
  }
}

export class HostAdmission implements ProvisioningAdmission {
  private readonly registry: SaasRegistry;
  private readonly config: SaasHostConfig;
  private readonly now: () => number;
  private readonly ratio: () => number;
  private readonly onAlert: (message: string) => void;

  constructor(options: HostAdmissionOptions) {
    this.registry = options.registry;
    this.config = options.config;
    this.now = options.now ?? Date.now;
    this.ratio = options.diskRatio ?? diskUsedRatio;
    this.onAlert = options.onAlert ?? (() => {});
  }

  private assertFreshBackups(): void {
    const timestamp = this.now();
    for (const coordinator of this.registry.listCoordinators(["active"])) {
      const backupDir = join(this.config.rootDir, "backups", coordinator.id);
      const latest = existsSync(backupDir)
        ? readdirSync(backupDir)
          .filter((name) => name.endsWith(".tar.age"))
          .map((name) => statSync(join(backupDir, name)).mtimeMs)
          .sort((a, b) => b - a)[0]
        : undefined;
      if (latest === undefined || timestamp - latest > BACKUP_MAX_AGE_MS) {
        throw new Error(`active coordinator ${coordinator.id} has no backup newer than 26 hours`);
      }
    }
  }
  private assertCapacityAvailable(): void {
    const admitted = this.registry.listAccounts()
      .filter((account) => account.state === "pending" || account.state === "active")
      .length;
    if (admitted >= this.config.maxAccounts) throw new HostCapacityError();
  }

  private assertNewReservationAllowed(): void {
    this.assertCapacityAvailable();
    assertDisk(this.ratio(), this.onAlert);
    this.assertFreshBackups();
  }


  async assertBeforeReservation<T>(reservation: () => T): Promise<T> {
    const owner = `admission-${randomUUID()}`;
    const deadline = Date.now() + ADMISSION_LEASE_MS;
    for (;;) {
      try {
        this.registry.acquireGlobalLease(
          ADMISSION_LEASE_RESOURCE,
          "reserve-account",
          owner,
          ADMISSION_LEASE_MS,
        );
        break;
      } catch (error) {
        if (!(error instanceof SaasRegistryError)
          || error.code !== "lease-held"
          || Date.now() >= deadline) throw error;
        await Bun.sleep(25);
      }
    }
    try {
      this.assertNewReservationAllowed();
      return reservation();
    } finally {
      this.registry.releaseGlobalLease(
        ADMISSION_LEASE_RESOURCE,
        "reserve-account",
        owner,
      );
    }
  }

  async assertPendingWorkAllowed(): Promise<void> {
    assertDisk(this.ratio(), this.onAlert);
    this.assertFreshBackups();
  }
}
