/**
 * This test isolates concurrent provisioning against the shared route snapshot boundary.
 * Its gated verifier proves account creation serializes without losing either hostname.
 * Shared cleanup and identities match the rest of the provisioning lifecycle suite.
 */
import { afterEach, describe, expect, test } from "bun:test";
import { join } from "node:path";
import { SaasRegistry } from "../src/saas/registry.ts";
import {
  SaasLifecycle,
  type ManagedRuntimePort,
  type TenantRouteManager,
} from "../src/saas/lifecycle.ts";
import {
  ACCOUNT_ID,
  cleanupProvisioningFixtures,
  COORDINATOR_ID,
  FakeAdmission,
  IMAGE,
  makeProvisioningRoot,
} from "./saas-provisioning-fixtures.ts";

afterEach(cleanupProvisioningFixtures);

describe("SaaS provisioning lifecycle", () => {
  test("concurrent account creates serialize shared route snapshots and retain both hosts", async () => {
    const root = makeProvisioningRoot("roost-saas-concurrent-");
    const accountB = "33333333-3333-4333-8333-333333333333";
    const coordinatorB = "44444444-4444-4444-8444-444444444444";
    const ids = [ACCOUNT_ID, COORDINATOR_ID, accountB, coordinatorB];
    const registry = new SaasRegistry({
      rootDir: root,
      path: join(root, "control.db"),
      now: () => 1_000,
      createId: () => {
        const id = ids.shift();
        if (!id) throw new Error("test UUID source exhausted");
        return id;
      },
    });
    const routeSnapshots: string[][] = [];
    const firstVerifyEntered = Promise.withResolvers<void>();
    const releaseFirstVerify = Promise.withResolvers<void>();
    let verifyCount = 0;
    const routes: TenantRouteManager = {
      reconcile: async (rows) => { routeSnapshots.push(rows.map((row) => row.id).sort()); },
      verify: async () => {
        verifyCount++;
        if (verifyCount === 1) {
          firstVerifyEntered.resolve();
          await releaseFirstVerify.promise;
        }
      },
      verifyResolver: async () => {},
    };
    const runtime: ManagedRuntimePort = {
      seedOwnerActivation: async () => {},
      seedSignupGatewayOwnerActivation: async () => {},
      seedGoogleOwner: async () => {},
      ensureContainer: async () => {},
      startAndVerify: async () => {},
      stop: async () => {},
      releaseOwnerActivationEmail: async () => {},
      activationStatus: async (coordinator) => ({
        activated: false,
        accountId: coordinator.accountId,
        coordinatorId: coordinator.id,
        expiresAtMs: 10_000,
        topology: "pending-coordinator-email",
      }),
    };
    const lifecycle = new SaasLifecycle({
      registry,
      runtime,
      routes,
      admission: new FakeAdmission(),
      email: {
        resendEndpoint: "https://api.resend.com/emails",
        emailFrom: "noreply@example.com",
        sharedResendApiKeyPath: join(root, "resend-key"),
      },
      authVerifyKeyFile: join(root, "saas-auth-verify-key"),
    });
    try {
      const first = lifecycle.accountCreate("owner-a@example.com", IMAGE);
      const second = lifecycle.accountCreate("owner-b@example.com", IMAGE);
      await firstVerifyEntered.promise;
      releaseFirstVerify.resolve();
      const results = await Promise.all([first, second]);
      expect(results.map((result) => result.coordinator.state)).toEqual(["invited", "invited"]);
      expect(registry.listAccounts()).toHaveLength(2);
      expect(registry.listCoordinators()).toHaveLength(2);
      expect(routeSnapshots.at(-1)).toEqual([COORDINATOR_ID, coordinatorB].sort());
    } finally {
      registry.close();
    }
  });
});
