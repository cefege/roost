import { Database } from "bun:sqlite";
import { afterEach, describe, expect, test } from "bun:test";
import { mkdirSync, mkdtempSync, rmSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { SaasRegistry, type RegistryCoordinator } from "../src/saas/registry.ts";
import { SaasRollout, type RolloutRuntimePort } from "../src/saas/rollout.ts";
import type { ActivationStatus, ManagedInstanceSpec } from "../src/saas/docker.ts";
import type { ProvisioningAdmission, TenantRouteManager } from "../src/saas/lifecycle.ts";

const ACCOUNT_ID = "11111111-1111-4111-8111-111111111111";
const COORDINATOR_ID = "22222222-2222-4222-8222-222222222222";
const OLD_IMAGE = `sha256:${"a".repeat(64)}`;
const NEW_IMAGE = `sha256:${"b".repeat(64)}`;
const cleanups: string[] = [];
afterEach(() => {
  while (cleanups.length > 0) rmSync(cleanups.pop()!, { recursive: true, force: true });
});

class RolloutRuntime implements RolloutRuntimePort {
  calls: string[] = [];
  failReplacement = false;

  async seedOwnerActivation(_spec: ManagedInstanceSpec): Promise<void> { throw new Error("not used"); }
  async seedSignupGatewayOwnerActivation(
    _spec: ManagedInstanceSpec,
    _activationTokenHash: string,
  ): Promise<void> {
    throw new Error("not used");
  }
  async seedGoogleOwner(_spec: ManagedInstanceSpec, _googleSubject: string): Promise<void> {
    throw new Error("not used");
  }
  async ensureContainer(spec: ManagedInstanceSpec): Promise<void> { this.calls.push(`ensure:${spec.coordinator.imageDigest}`); }
  async startAndVerify(spec: ManagedInstanceSpec): Promise<void> {
    this.calls.push(`start:${spec.coordinator.imageDigest}`);
    if (this.failReplacement && spec.coordinator.imageDigest === NEW_IMAGE) throw new Error("replacement unhealthy");
  }
  async stop(_coordinator: RegistryCoordinator): Promise<void> { this.calls.push("stop-old"); }
  async releaseOwnerActivationEmail(_coordinator: RegistryCoordinator): Promise<void> { throw new Error("not used"); }
  async activationStatus(_coordinator: RegistryCoordinator): Promise<ActivationStatus> { throw new Error("not used"); }
  async renameContainer(from: string, to: string): Promise<void> { this.calls.push(`rename:${from}:${to}`); }
  async removeContainer(name: string): Promise<void> { this.calls.push(`remove:${name}`); }
}

class RolloutRoutes implements TenantRouteManager {
  calls: string[] = [];
  async reconcile(rows: readonly RegistryCoordinator[]): Promise<void> {
    this.calls.push(`routes:${rows.map((row) => row.imageDigest).join(",")}`);
  }
  async verify(row: RegistryCoordinator): Promise<void> { this.calls.push(`verify:${row.imageDigest}`); }
  async verifyResolver(): Promise<void> { throw new Error("not used"); }
}

class RolloutAdmission implements ProvisioningAdmission {
  checks = 0;
  async assertBeforeReservation<T>(reservation: () => T): Promise<T> {
    this.checks++;
    return reservation();
  }
  async assertPendingWorkAllowed(): Promise<void> { this.checks++; }
}

function fixture() {
  const root = mkdtempSync(join(tmpdir(), "roost-saas-rollout-"));
  cleanups.push(root);
  const ids = [ACCOUNT_ID, COORDINATOR_ID];
  const registry = new SaasRegistry({
    rootDir: root,
    path: join(root, "control.db"),
    now: () => 1_000,
    createId: () => ids.shift()!,
  });
  const reservation = registry.reserveAccount("owner@example.com", OLD_IMAGE);
  registry.markAccountActive(ACCOUNT_ID);
  registry.transitionCoordinator(COORDINATOR_ID, "reserved", "active");
  mkdirSync(reservation.coordinator.dataDir, { recursive: true, mode: 0o700 });
  const database = new Database(join(reservation.coordinator.dataDir, "coordinator_v2.db"));
  database.exec("CREATE TABLE proof (value TEXT); INSERT INTO proof VALUES ('before-rollout')");
  database.close(true);
  const runtime = new RolloutRuntime();
  const routes = new RolloutRoutes();
  const admission = new RolloutAdmission();
  const backups: string[] = [];
  const rollout = new SaasRollout({
    registry,
    runtime,
    routes,
    admission,
    email: {
      resendEndpoint: "https://api.resend.com/emails",
      emailFrom: "noreply@example.com",
      sharedResendApiKeyPath: join(root, "resend-key"),
    },
    authVerifyKeyFile: join(root, "saas-auth-verify-key"),
    backup: async (coordinator) => {
      backups.push(coordinator.id);
      return join(root, "backup.tar.age");
    },
    leaseOwner: () => "rollout-owner",
  });
  return { registry, reservation, runtime, routes, admission, backups, rollout };
}

describe("SaaS rollout", () => {
  test("backs up, removes route, stops old image, verifies replacement, then persists digest", async () => {
    const opened = fixture();
    try {
      const result = await opened.rollout.rollout(NEW_IMAGE);
      expect(result).toEqual([expect.objectContaining({
        coordinatorId: COORDINATOR_ID,
        priorDigest: OLD_IMAGE,
        imageDigest: NEW_IMAGE,
        rolledBack: false,
      })]);
      expect(opened.backups).toEqual([COORDINATOR_ID]);
      expect(opened.runtime.calls[0]).toBe("stop-old");
      expect(opened.runtime.calls).toContain(`ensure:${NEW_IMAGE}`);
      expect(opened.runtime.calls).toContain(`start:${NEW_IMAGE}`);
      expect(opened.registry.getCoordinator(COORDINATOR_ID).imageDigest).toBe(NEW_IMAGE);
      expect(opened.routes.calls[0]).toBe("routes:");
      expect(opened.routes.calls).toContain(`verify:${NEW_IMAGE}`);
      expect(opened.registry.getLease(COORDINATOR_ID)).toBeNull();
    } finally {
      opened.registry.close();
    }
  });

  test("removes a failed replacement before restarting and re-routing the prior digest", async () => {
    const opened = fixture();
    try {
      opened.runtime.failReplacement = true;
      const result = await opened.rollout.rollout(NEW_IMAGE);
      expect(result).toEqual([expect.objectContaining({
        imageDigest: OLD_IMAGE,
        rolledBack: true,
        error: "replacement unhealthy",
      })]);
      const replacementStart = opened.runtime.calls.indexOf(`start:${NEW_IMAGE}`);
      const removeReplacement = opened.runtime.calls.findIndex((call) => call.startsWith("remove:roost-coord-"));
      const oldStart = opened.runtime.calls.indexOf(`start:${OLD_IMAGE}`);
      expect(replacementStart).toBeGreaterThan(-1);
      expect(removeReplacement).toBeGreaterThan(replacementStart);
      expect(oldStart).toBeGreaterThan(removeReplacement);
      expect(opened.registry.getCoordinator(COORDINATOR_ID).imageDigest).toBe(OLD_IMAGE);
      expect(opened.routes.calls.at(-1)).toBe(`verify:${OLD_IMAGE}`);
    } finally {
      opened.registry.close();
    }
  });
});
