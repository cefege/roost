// Owns the single-worker lease, retry schedule, and durable job completion loop.
// Submission handling kicks this loop after inserting idempotent registry work.
// Global and row leases prevent duplicate lifecycle side effects after interruption.
import { randomUUID } from "node:crypto";
import {
  SaasRegistryError,
  type ClaimedProvisioningJob,
  type SaasRegistry,
} from "./registry.ts";
import type { ProvisioningAdmission, SaasLifecycle } from "./lifecycle.ts";
import {
  DEFAULT_JOB_LEASE_MS,
  DEFAULT_RETRY_DELAY_MS,
  MAX_JOB_ATTEMPTS,
  MAX_RETRY_DELAY_MS,
  WORKER_LEASE_RESOURCE,
  ordinalOneReservation,
  safeError,
  type ProvisioningWorkerOptions,
} from "./provisioning-contract.ts";

const MAX_TIMER_DELAY_MS = 2_147_483_647;

export class ProvisioningJobLoop {
  protected readonly registry: SaasRegistry;
  protected readonly lifecycle: SaasLifecycle;
  protected readonly admission: ProvisioningAdmission;
  protected readonly imageDigest: string;
  protected readonly now: () => number;
  protected readonly leaseDurationMs: number;
  protected readonly retryDelayMs: number;
  private processing = false;
  private drainPromise: Promise<void> | null = null;
  private wakeScheduling = false;
  private wakeTimer: NodeJS.Timeout | undefined;

  constructor(options: ProvisioningWorkerOptions) {
    this.registry = options.registry;
    this.lifecycle = options.lifecycle;
    this.admission = options.admission;
    this.imageDigest = options.imageDigest;
    this.now = options.now ?? Date.now;
    this.leaseDurationMs = options.leaseDurationMs ?? DEFAULT_JOB_LEASE_MS;
    this.retryDelayMs = options.retryDelayMs ?? DEFAULT_RETRY_DELAY_MS;
    if (!Number.isSafeInteger(this.leaseDurationMs) || this.leaseDurationMs < 1) {
      throw new Error("provisioning lease duration is invalid");
    }
    if (!Number.isSafeInteger(this.retryDelayMs) || this.retryDelayMs < 1) {
      throw new Error("provisioning retry delay is invalid");
    }
  }

  start(): void {
    if (this.wakeScheduling) return;
    this.wakeScheduling = true;
    this.kick();
  }

  async stop(): Promise<void> {
    this.wakeScheduling = false;
    clearTimeout(this.wakeTimer);
    this.wakeTimer = undefined;
    const draining = this.drainPromise;
    if (draining) await draining;
  }

  async _waitUntilIdle(): Promise<void> {
    const draining = this.drainPromise;
    if (draining) await draining;
  }


  async runOnce(): Promise<boolean> {
    if (this.processing) return false;
    this.processing = true;
    const owner = `worker-${randomUUID()}`;
    let globalLease = false;
    let leaseFailure: unknown = null;
    let heartbeat: NodeJS.Timeout | undefined;
    try {
      try {
        this.registry.acquireGlobalLease(
          WORKER_LEASE_RESOURCE,
          "process",
          owner,
          this.leaseDurationMs,
        );
        globalLease = true;
      } catch (error) {
        if (error instanceof SaasRegistryError && error.code === "lease-held") return false;
        throw error;
      }
      const heartbeatMs = Math.max(10, Math.min(60_000, Math.floor(this.leaseDurationMs / 3)));
      heartbeat = setInterval(() => {
        try {
          this.registry.renewGlobalLease(
            WORKER_LEASE_RESOURCE,
            "process",
            owner,
            this.leaseDurationMs,
          );
        } catch (error) {
          leaseFailure = error;
        }
      }, heartbeatMs);
      heartbeat.unref();
      const [job] = this.registry.claimDueProvisioningJobs({
        leaseDurationMs: this.leaseDurationMs,
        limit: 1,
      });
      if (!job) return false;
      await this.processClaim(job);
      if (leaseFailure) throw leaseFailure;
      this.registry.renewGlobalLease(
        WORKER_LEASE_RESOURCE,
        "process",
        owner,
        this.leaseDurationMs,
      );
      return true;
    } finally {
      clearInterval(heartbeat);
      if (globalLease) {
        try {
          this.registry.releaseGlobalLease(WORKER_LEASE_RESOURCE, "process", owner);
        } catch {
          // A stale worker must never clear a successor's lease.
        }
      }
      this.processing = false;
    }
  }

  kick(): void {
    clearTimeout(this.wakeTimer);
    this.wakeTimer = undefined;
    if (this.drainPromise) return;
    this.drainPromise = this.drain()
      .catch(() => {
        console.error(JSON.stringify({ event: "saas.provisioner_worker_stopped", error: "worker lease lost" }));
      })
      .finally(() => {
        this.drainPromise = null;
        if (this.wakeScheduling) this.scheduleNextWake();
      });
  }

  private scheduleNextWake(): void {
    let delayMs: number;
    try {
      const jobWakeAtMs = this.registry.nextProvisioningJobWakeAtMs();
      if (jobWakeAtMs === null) return;
      const timestamp = this.now();
      const workerLease = this.registry.getGlobalLease(WORKER_LEASE_RESOURCE);
      const wakeAtMs = workerLease && workerLease.expiresAtMs > timestamp
        ? Math.max(jobWakeAtMs, workerLease.expiresAtMs)
        : jobWakeAtMs;
      delayMs = Math.max(0, Math.min(MAX_TIMER_DELAY_MS, wakeAtMs - timestamp));
    } catch {
      delayMs = this.retryDelayMs;
      console.error(JSON.stringify({
        event: "saas.provisioner_worker_wake_deferred",
        error: "worker schedule unavailable",
      }));
    }
    this.wakeTimer = setTimeout(() => {
      this.wakeTimer = undefined;
      this.kick();
    }, delayMs);
    this.wakeTimer.unref();
  }

  private async drain(): Promise<void> {
    while (await this.runOnce()) {
      // claimDue() returns at most one row, preserving a single side-effect worker.
    }
  }

  private async processClaim(job: ClaimedProvisioningJob): Promise<void> {
    try {
      if (job.kind === "verified-email") {
        if (!job.activationTokenHash) throw new Error("verified-email job is missing activation proof");
        const result = await this.lifecycle.provision(
          ordinalOneReservation(this.registry, job.accountId, job.coordinatorId, true),
          { kind: "verified-email", activationTokenHash: job.activationTokenHash },
        );
        if (result.coordinator.state !== "invited" && result.coordinator.state !== "active") {
          throw new Error("verified-email lifecycle did not reach a safe state");
        }
      } else if (job.kind === "google-signup") {
        if (!job.identitySubject) throw new Error("Google signup job is missing its subject");
        const result = await this.lifecycle.provision(
          ordinalOneReservation(this.registry, job.accountId, job.coordinatorId, true),
          { kind: "google", subject: job.identitySubject },
        );
        if (result.account.state !== "active" || result.coordinator.state !== "active") {
          throw new Error("Google lifecycle did not reach active state");
        }
      } else if (job.kind === "google-login") {
        await this.lifecycle.proveActive(
          job.accountId,
          job.coordinatorId,
          ["active-passwordless-google", "active-linked"],
        );
      } else {
        await this.lifecycle.proveActive(job.accountId, job.coordinatorId);
      }
      if (!this.registry.markProvisioningJobSucceeded(job.id, job.leaseToken, job.assertionInput)) {
        throw new Error("provisioning job lease was lost before completion");
      }
    } catch (error) {
      const message = safeError(error);
      if (job.attempts >= MAX_JOB_ATTEMPTS || (error instanceof SaasRegistryError
        && (error.code === "invalid" || error.code === "conflict" || error.code === "not-found"))) {
        this.registry.markProvisioningJobFailed(job.id, job.leaseToken, message);
        return;
      }
      const multiplier = 2 ** Math.min(job.attempts - 1, 6);
      const delay = Math.min(MAX_RETRY_DELAY_MS, this.retryDelayMs * multiplier);
      this.registry.rescheduleProvisioningJob(job.id, job.leaseToken, this.now() + delay, message);
    }
  }
}
