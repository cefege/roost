/**
 * These tests pin initial SaaS provisioning, recovery, and activation transitions.
 * Effect ordering and durable checkpoints protect retries from repeating completed work.
 * Shared lifecycle fakes keep the assertions focused on observable provisioning behavior.
 */
import { afterEach, describe, expect, test, vi } from "bun:test";
import { ProvisioningWorker } from "../src/saas/provisioning-submission-worker.ts";
import {
  ACCOUNT_ID,
  cleanupProvisioningFixtures,
  COORDINATOR_ID,
  fixture,
  IMAGE,
} from "./saas-provisioning-fixtures.ts";


afterEach(cleanupProvisioningFixtures);

describe("SaaS provisioning lifecycle", () => {
  test("reserves before effects and advances one durable state per idempotent effect", async () => {
    const opened = fixture();
    try {
      const result = await opened.lifecycle.accountCreate("owner@example.com", IMAGE);
      expect(result.resumed).toBe(false);
      expect(result.account.state).toBe("pending");
      expect(result.coordinator.state).toBe("invited");
      expect(opened.runtime.calls).toEqual([
        "seed",
        "ensure-container",
        "start-verify",
        "ensure-container",
        "start-verify",
        "ensure-container",
        "start-verify",
        "activation-status",
        "release-email",
      ]);
      expect(opened.routes.calls).toEqual([
        "routes-reconcile",
        "routes-verify",
        "routes-reconcile",
        "routes-verify",
        "resolver-verify",
      ]);
      expect(opened.registry.getCoordinator(COORDINATOR_ID)).toEqual(expect.objectContaining({
        state: "invited",
        seededAtMs: 1_000,
        runningAtMs: 1_000,
        routedAtMs: 1_000,
        invitedAtMs: 1_000,
      }));
    } finally {
      opened.registry.close();
    }
  });

  test("resumes the last safe state without repeating completed effects", async () => {
    const opened = fixture();
    try {
      opened.runtime.failAt = "start-verify";
      await expect(opened.lifecycle.accountCreate("owner@example.com", IMAGE)).rejects.toThrow("start-verify");
      expect(opened.registry.getCoordinator(COORDINATOR_ID).state).toBe("seeded");
      expect(opened.registry.getCoordinator(COORDINATOR_ID).lastError).not.toContain("owner@example.com");
      expect(opened.registry.getCoordinator(COORDINATOR_ID).lastError).not.toContain("must-redact");
      opened.runtime.calls = [];
      opened.runtime.failAt = null;
      const resumed = await opened.lifecycle.accountCreate("owner@example.com", IMAGE);
      expect(resumed.resumed).toBe(true);
      expect(opened.runtime.calls).not.toContain("seed");
      expect(resumed.coordinator.state).toBe("invited");
      expect(opened.registry.listAccounts()).toHaveLength(1);
      expect(opened.registry.listCoordinators()).toHaveLength(1);
    } finally {
      opened.registry.close();
    }
  });

  test("restores the prior route set and keeps email held on route proof failure", async () => {
    const opened = fixture();
    try {
      opened.routes.failVerify = true;
      await expect(opened.lifecycle.accountCreate("owner@example.com", IMAGE)).rejects.toThrow("route verification");
      expect(opened.registry.getCoordinator(COORDINATOR_ID).state).toBe("running");
      expect(opened.runtime.calls).not.toContain("release-email");
      expect(opened.routes.desired).toEqual([[COORDINATOR_ID], []]);
    } finally {
      opened.registry.close();
    }
  });

  test("resolver outage keeps owner email held in recoverable routed state", async () => {
    const opened = fixture();
    try {
      opened.routes.failResolverVerify = true;
      await expect(opened.lifecycle.accountCreate("owner@example.com", IMAGE))
        .rejects.toThrow("resolver verification");
      expect(opened.registry.getAccount(ACCOUNT_ID).state).toBe("pending");
      expect(opened.registry.getCoordinator(COORDINATOR_ID).state).toBe("routed");
      expect(opened.runtime.calls).not.toContain("release-email");

      opened.routes.failResolverVerify = false;
      opened.runtime.calls = [];
      const reconciled = await opened.lifecycle.reconcile();
      expect(reconciled[0]).toEqual(expect.objectContaining({ state: "invited", repaired: true }));
      expect(opened.runtime.calls).toContain("release-email");
    } finally {
      opened.registry.close();
    }
  });

  test("preflight failure occurs before registry reservation", async () => {
    const opened = fixture();
    try {
      opened.admission.blocked = true;
      await expect(opened.lifecycle.accountCreate("owner@example.com", IMAGE)).rejects.toThrow("85%");
      expect(opened.registry.listAccounts()).toHaveLength(0);
      expect(opened.runtime.calls).toHaveLength(0);
    } finally {
      opened.registry.close();
    }
  });

  test("marks the exact account active only after instance status proves commit", async () => {
    const opened = fixture();
    try {
      await opened.lifecycle.accountCreate("owner@example.com", IMAGE);
      opened.runtime.status = {
        ...opened.runtime.status,
        activated: true,
        topology: "active-native-password",
      };
      const results = await opened.lifecycle.reconcile();
      expect(results).toEqual([expect.objectContaining({ state: "active", repaired: true })]);
      expect(opened.registry.getAccount(ACCOUNT_ID).state).toBe("active");
      expect(opened.registry.getCoordinator(COORDINATOR_ID).state).toBe("active");
    } finally {
      opened.registry.close();
    }
  });

  test("restart reconciliation advances seeded and running coordinators without reseeding", async () => {
    for (const initialState of ["seeded", "running"] as const) {
      const opened = fixture();
      try {
        const reservation = opened.registry.reserveAccount("owner@example.com", IMAGE);
        opened.registry.transitionCoordinator(reservation.coordinator.id, "reserved", "seeded");
        if (initialState === "running") {
          opened.registry.transitionCoordinator(reservation.coordinator.id, "seeded", "running");
        }
        opened.runtime.calls = [];
        opened.routes.calls = [];

        const results = await opened.lifecycle.reconcile();

        expect(results).toEqual([
          expect.objectContaining({ state: "invited", repaired: true }),
        ]);
        expect(opened.registry.getCoordinator(COORDINATOR_ID).state).toBe("invited");
        expect(opened.runtime.calls).not.toContain("seed");
        expect(opened.runtime.calls).toContain("start-verify");
        expect(opened.routes.calls).toContain("routes-verify");
        expect(opened.routes.calls).toContain("resolver-verify");
      } finally {
        opened.registry.close();
      }
    }
  });

  test("worker startup drains a persisted job without a new submission or status poll", async () => {
    const opened = fixture();
    const reservation = opened.registry.reserveAccount("owner@example.com", IMAGE);
    const insertion = opened.registry.insertProvisioningJob({
      idempotencyKeyHash: "c".repeat(64),
      kind: "verified-email",
      emailNormalized: reservation.account.emailNormalized,
      activationTokenHash: "d".repeat(64),
      verifiedAtMs: opened.nowRef.value,
      accountId: reservation.account.id,
      coordinatorId: reservation.coordinator.id,
    });
    const worker = new ProvisioningWorker({
      registry: opened.registry,
      lifecycle: opened.lifecycle,
      admission: opened.admission,
      imageDigest: IMAGE,
      now: () => opened.nowRef.value,
      leaseDurationMs: 100,
      retryDelayMs: 50,
    });
    try {
      worker.start();
      await worker._waitUntilIdle();
      const completed = opened.registry.getProvisioningJob(insertion.job.id);
      expect(completed.state).toBe("succeeded");
      expect(completed.attempts).toBe(1);
      expect(opened.registry.getCoordinator(COORDINATOR_ID).state).toBe("invited");
      expect(opened.runtime.calls[0]).toBe("seed-signup-gateway");
    } finally {
      await worker.stop();
      opened.registry.close();
    }
  });

  test("worker wakes a future-due retry without a request polling its status", async () => {
    const opened = fixture();
    const reservation = opened.registry.reserveAccount("owner@example.com", IMAGE);
    const insertion = opened.registry.insertProvisioningJob({
      idempotencyKeyHash: "e".repeat(64),
      kind: "verified-email",
      emailNormalized: reservation.account.emailNormalized,
      activationTokenHash: "f".repeat(64),
      verifiedAtMs: opened.nowRef.value,
      accountId: reservation.account.id,
      coordinatorId: reservation.coordinator.id,
    });
    opened.runtime.failAt = "start-verify";
    const worker = new ProvisioningWorker({
      registry: opened.registry,
      lifecycle: opened.lifecycle,
      admission: opened.admission,
      imageDigest: IMAGE,
      now: () => opened.nowRef.value,
      leaseDurationMs: 100,
      retryDelayMs: 50,
    });
    vi.useFakeTimers();
    try {
      worker.start();
      await worker._waitUntilIdle();
      const deferred = opened.registry.getProvisioningJob(insertion.job.id);
      expect(deferred).toEqual(expect.objectContaining({ state: "pending", attempts: 1 }));
      opened.runtime.failAt = null;
      opened.nowRef.value = deferred.nextAttemptAtMs;
      vi.advanceTimersByTime(50);
      await worker._waitUntilIdle();
      const completed = opened.registry.getProvisioningJob(insertion.job.id);
      expect(completed.state).toBe("succeeded");
      expect(completed.attempts).toBe(2);
    } finally {
      await worker.stop();
      opened.registry.close();
      vi.useRealTimers();
    }
  });
});
