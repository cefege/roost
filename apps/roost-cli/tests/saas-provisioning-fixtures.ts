/**
 * Shared provisioning fakes keep lifecycle tests focused on state and ordering assertions.
 * Each test module uses the same deterministic identities, clock, and temporary-root cleanup.
 * The fakes implement production ports so interface changes remain visible to these tests.
 */
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import {
  SaasRegistry,
  type RegistryAccount,
  type RegistryCoordinator,
} from "../src/saas/registry.ts";
import {
  SaasLifecycle,
  type ManagedRuntimePort,
  type ProvisioningAdmission,
  type TenantRouteManager,
} from "../src/saas/lifecycle.ts";
import type { ActivationStatus, ManagedInstanceSpec } from "../src/saas/docker.ts";

export const ACCOUNT_ID = "11111111-1111-4111-8111-111111111111";
export const COORDINATOR_ID = "22222222-2222-4222-8222-222222222222";
export const JOB_ID = "33333333-3333-4333-8333-333333333333";
export const SECOND_JOB_ID = "44444444-4444-4444-8444-444444444444";
export const IMAGE = `sha256:${"a".repeat(64)}`;
const cleanups: string[] = [];

export function cleanupProvisioningFixtures(): void {
  while (cleanups.length > 0) rmSync(cleanups.pop()!, { recursive: true, force: true });
}

export function makeProvisioningRoot(prefix: string): string {
  const root = mkdtempSync(join(tmpdir(), prefix));
  cleanups.push(root);
  return root;
}

class FakeRuntime implements ManagedRuntimePort {
  calls: string[] = [];
  status: ActivationStatus = {
    activated: false,
    accountId: ACCOUNT_ID,
    coordinatorId: COORDINATOR_ID,
    expiresAtMs: 10_000,
    topology: "pending-coordinator-email",
  };
  failAt: string | null = null;

  private record(name: string): void {
    this.calls.push(name);
    if (this.failAt === name) throw new Error(`${name} token=must-redact owner@example.com`);
  }

  async seedOwnerActivation(_spec: ManagedInstanceSpec): Promise<void> { this.record("seed"); }
  async seedSignupGatewayOwnerActivation(
    _spec: ManagedInstanceSpec,
    _activationTokenHash: string,
  ): Promise<void> {
    this.record("seed-signup-gateway");
  }
  async seedGoogleOwner(_spec: ManagedInstanceSpec, _googleSubject: string): Promise<void> {
    this.record("seed-google");
  }
  async ensureContainer(_spec: ManagedInstanceSpec): Promise<void> { this.record("ensure-container"); }
  async startAndVerify(_spec: ManagedInstanceSpec): Promise<void> { this.record("start-verify"); }
  async stop(_coordinator: RegistryCoordinator): Promise<void> { this.record("stop"); }
  async releaseOwnerActivationEmail(_coordinator: RegistryCoordinator): Promise<void> { this.record("release-email"); }
  async activationStatus(_coordinator: RegistryCoordinator): Promise<ActivationStatus> {
    this.record("activation-status");
    return this.status;
  }
}

class FakeRoutes implements TenantRouteManager {
  calls: string[] = [];
  desired: string[][] = [];
  failVerify = false;
  failResolverVerify = false;

  async reconcile(coordinators: readonly RegistryCoordinator[]): Promise<void> {
    this.calls.push("routes-reconcile");
    this.desired.push(coordinators.map((row) => row.id));
  }

  async verify(_coordinator: RegistryCoordinator): Promise<void> {
    this.calls.push("routes-verify");
    if (this.failVerify) throw new Error("public route verification failed");
  }

  async verifyResolver(_account: RegistryAccount): Promise<void> {
    this.calls.push("resolver-verify");
    if (this.failResolverVerify) throw new Error("shared resolver verification failed");
  }
}

export class FakeAdmission implements ProvisioningAdmission {
  before = 0;
  pending = 0;
  blocked = false;

  async assertBeforeReservation<T>(reserve: () => T): Promise<T> {
    this.before++;
    if (this.blocked) throw new Error("host disk is at or above 85%");
    return reserve();
  }

  async assertPendingWorkAllowed(): Promise<void> {
    this.pending++;
    if (this.blocked) throw new Error("host disk is at or above 85%");
  }
}

export function fixture(nowRef = { value: 1_000 }) {
  const root = makeProvisioningRoot("roost-saas-lifecycle-");
  const ids = [ACCOUNT_ID, COORDINATOR_ID, JOB_ID, SECOND_JOB_ID];
  const registry = new SaasRegistry({
    rootDir: root,
    path: join(root, "control.db"),
    now: () => nowRef.value,
    createId: () => {
      const id = ids.shift();
      if (!id) throw new Error("test UUID source exhausted");
      return id;
    },
  });
  const runtime = new FakeRuntime();
  const routes = new FakeRoutes();
  const admission = new FakeAdmission();
  const lifecycle = new SaasLifecycle({
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
    now: () => nowRef.value,
    leaseOwner: () => "test-owner",
  });
  return { root, registry, runtime, routes, admission, lifecycle, nowRef };
}
