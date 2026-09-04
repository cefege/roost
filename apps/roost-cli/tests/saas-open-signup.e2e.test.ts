// This E2E test proves that open-signup reservations become isolated managed owners.
// It drives concurrent submissions, restart recovery, and the final tenant databases.
// Shared Docker and registry fixtures live beside the scenario to keep the flow readable.

import { expect, test } from "bun:test";
import { Database } from "bun:sqlite";
import {
  mkdtempSync,
  readFileSync,
  rmSync,
  statSync,
  writeFileSync,
} from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import {
  GOOGLE_IDENTITY_ISSUER,
  SaasRegistry,
} from "../src/saas/registry.ts";
import {
  SaasLifecycle,
  type ProvisioningAdmission,
  type TenantRouteManager,
} from "../src/saas/lifecycle.ts";
import { ManagedInstanceRuntime } from "../src/saas/docker.ts";
import { runSignupInit } from "../src/saas-auth/signup-init.ts";
import {
  ACTIVATION_HASH,
  EMAIL_OWNER,
  GOOGLE_EMAIL_A,
  GOOGLE_EMAIL_B,
  GOOGLE_SUBJECT_A,
  GOOGLE_SUBJECT_B,
  ManualProvisioningWorker,
  assertGoogleOwnerDatabase,
  assertSignupGatewayDatabase,
  docker,
  dockerCleanup,
  enabled,
  instanceSpec,
  pending,
  proveCapacityBoundary,
  proveLeaseReclaimAndIdempotency,
} from "./saas-open-signup-fixture.ts";
import { requiredManagedE2eResources } from "./managed-e2e-fixture.ts";

test.skipIf(!enabled)("converges open-signup reservations into isolated restart-safe managed owners", async () => {
  if (typeof process.getuid !== "function" || process.getuid() !== 0) {
    throw new Error("ROOST_SIGNUP_E2E requires Linux root");
  }
  const { imageId, network } = requiredManagedE2eResources();
  const root = mkdtempSync(join(tmpdir(), "roost-open-signup-e2e-"));
  const sharedResendApiKeyPath = join(root, "resend-api-key");
  let registry: SaasRegistry | null = null;
  const containerNames = new Set<string>();
  try {
    writeFileSync(sharedResendApiKeyPath, "re_signup_e2e_not_delivered", { mode: 0o600 });
    const initialized = runSignupInit({ credentialDirectory: join(root, "saas-auth") });
    expect(statSync(initialized.assertionSigningKeyPath).mode & 0o777).toBe(0o600);
    expect(statSync(initialized.assertionVerifyKeyPath).mode & 0o777).toBe(0o644);
    expect(readFileSync(initialized.assertionVerifyKeyPath, "utf8")).toContain("ssh-ed25519 ");

    docker(["network", "inspect", network]);

    registry = new SaasRegistry({ rootDir: root, path: join(root, "control.db") });
    const runtime = new ManagedInstanceRuntime({ network });
    const admission: ProvisioningAdmission = {
      async assertBeforeReservation<T>(reservation: () => T): Promise<T> {
        return reservation();
      },
      async assertPendingWorkAllowed(): Promise<void> {},
    };
    const routeSnapshots: string[][] = [];
    const routes: TenantRouteManager = {
      async reconcile(coordinators): Promise<void> {
        const routeKeys = coordinators.map((coordinator) => coordinator.routeKey);
        if (new Set(routeKeys).size !== routeKeys.length) throw new Error("duplicate disposable route key");
        if (new Set(coordinators.map((coordinator) => coordinator.containerName)).size !== coordinators.length) {
          throw new Error("duplicate disposable route target");
        }
        routeSnapshots.push([...routeKeys].sort());
      },
      async verify(coordinator): Promise<void> {
        if (await runtime.containerHealth(coordinator) !== "healthy") {
          throw new Error("disposable route target was not healthy");
        }
      },
      async verifyResolver(account): Promise<void> {
        if (registry!.getAccount(account.id).routeKey !== account.routeKey) {
          throw new Error("disposable resolver route binding mismatch");
        }
      },
    };
    const lifecycle = new SaasLifecycle({
      registry,
      runtime,
      routes,
      admission,
      authVerifyKeyFile: initialized.assertionVerifyKeyPath,
      email: {
        resendEndpoint: "https://api.resend.com/emails",
        emailFrom: "Roost signup E2E <noreply@signup-e2e.example>",
        sharedResendApiKeyPath,
      },
    });
    const worker = new ManualProvisioningWorker({
      registry,
      lifecycle,
      admission,
      imageDigest: imageId,
      leaseDurationMs: 60_000,
      retryDelayMs: 100,
    });

    const verifiedAtMs = Date.now();
    const sameGoogleInput = {
      kind: "google-signup" as const,
      issuer: GOOGLE_IDENTITY_ISSUER,
      subject: GOOGLE_SUBJECT_A,
      emailNormalized: GOOGLE_EMAIL_A,
      verifiedAtMs,
      expiresAtMs: verifiedAtMs + 10 * 60_000,
      idempotencyKey: "1".repeat(64),
    };
    const [sameFirst, sameSecond] = await Promise.all([
      worker.submit(sameGoogleInput),
      worker.submit(sameGoogleInput),
    ]);
    const firstGoogleJob = pending(sameFirst);
    const retriedGoogleJob = pending(sameSecond);
    expect(retriedGoogleJob.jobId).toBe(firstGoogleJob.jobId);
    expect(registry.listAccounts()).toHaveLength(1);
    expect(registry.listCoordinators()).toHaveLength(1);
    expect(await worker.runOnce()).toBe(true);
    expect(registry.getProvisioningJob(firstGoogleJob.jobId).state).toBe("succeeded");

    const secondVerifiedAtMs = Date.now();
    const [secondGoogleResult, emailResult] = await Promise.all([
      worker.submit({
        kind: "google-signup",
        issuer: GOOGLE_IDENTITY_ISSUER,
        subject: GOOGLE_SUBJECT_B,
        emailNormalized: GOOGLE_EMAIL_B,
        verifiedAtMs: secondVerifiedAtMs,
        expiresAtMs: secondVerifiedAtMs + 10 * 60_000,
        idempotencyKey: "2".repeat(64),
      }),
      worker.submit({
        kind: "verified-email",
        challengeId: "10000000-0000-4000-8000-000000000001",
        emailNormalized: EMAIL_OWNER,
        activationTokenHash: ACTIVATION_HASH,
        verifiedAtMs: secondVerifiedAtMs,
        expiresAtMs: secondVerifiedAtMs + 7 * 24 * 60 * 60_000,
        idempotencyKey: "3".repeat(64),
      }),
    ]);
    const secondGoogleJob = pending(secondGoogleResult);
    const emailJob = pending(emailResult);
    expect(await worker.runOnce()).toBe(true);
    expect([
      registry.getProvisioningJob(secondGoogleJob.jobId).state,
      registry.getProvisioningJob(emailJob.jobId).state,
    ].filter((state) => state === "succeeded")).toHaveLength(1);
    expect(await worker.runOnce()).toBe(true);
    expect(registry.getProvisioningJob(secondGoogleJob.jobId).state).toBe("succeeded");
    expect(registry.getProvisioningJob(emailJob.jobId).state).toBe("succeeded");
    expect(await worker.runOnce()).toBe(false);

    const firstJobRow = registry.getProvisioningJob(firstGoogleJob.jobId);
    const secondJobRow = registry.getProvisioningJob(secondGoogleJob.jobId);
    const emailJobRow = registry.getProvisioningJob(emailJob.jobId);
    const firstAccount = registry.getAccount(firstJobRow.accountId);
    const secondAccount = registry.getAccount(secondJobRow.accountId);
    const emailAccount = registry.getAccount(emailJobRow.accountId);
    const firstCoordinator = registry.getCoordinator(firstJobRow.coordinatorId);
    const secondCoordinator = registry.getCoordinator(secondJobRow.coordinatorId);
    const emailCoordinator = registry.getCoordinator(emailJobRow.coordinatorId);
    for (const coordinator of [firstCoordinator, secondCoordinator, emailCoordinator]) {
      containerNames.add(coordinator.containerName);
    }

    expect(registry.listAccounts()).toHaveLength(3);
    expect(registry.listCoordinators()).toHaveLength(3);
    expect(new Set([firstAccount.id, secondAccount.id, emailAccount.id]).size).toBe(3);
    expect(new Set([firstCoordinator.id, secondCoordinator.id, emailCoordinator.id]).size).toBe(3);
    expect(new Set([firstAccount.routeKey, secondAccount.routeKey, emailAccount.routeKey]).size).toBe(3);
    expect(registry.listCoordinators().filter((row) => row.accountId === firstAccount.id)).toEqual([
      firstCoordinator,
    ]);
    expect(registry.getFederatedIdentity(GOOGLE_IDENTITY_ISSUER, GOOGLE_SUBJECT_A)).toMatchObject({
      accountId: firstAccount.id,
      emailNormalized: GOOGLE_EMAIL_A,
      state: "active",
    });
    expect(registry.getFederatedIdentity(GOOGLE_IDENTITY_ISSUER, GOOGLE_SUBJECT_B)).toMatchObject({
      accountId: secondAccount.id,
      emailNormalized: GOOGLE_EMAIL_B,
      state: "active",
    });
    expect(worker.status(firstGoogleJob.jobId)).toMatchObject({
      state: "awaiting-device",
      routeKey: firstAccount.routeKey,
    });
    expect(worker.status(secondGoogleJob.jobId)).toMatchObject({
      state: "awaiting-device",
      routeKey: secondAccount.routeKey,
    });
    expect(worker.status(emailJob.jobId)).toEqual({
      state: "ready",
      routeKey: emailAccount.routeKey,
    });
    expect(routeSnapshots.length).toBeGreaterThan(0);

    const specs = [
      instanceSpec(registry, firstAccount.id, firstCoordinator.id, initialized.assertionVerifyKeyPath, sharedResendApiKeyPath),
      instanceSpec(registry, secondAccount.id, secondCoordinator.id, initialized.assertionVerifyKeyPath, sharedResendApiKeyPath),
      instanceSpec(registry, emailAccount.id, emailCoordinator.id, initialized.assertionVerifyKeyPath, sharedResendApiKeyPath),
    ];
    for (const spec of specs) await runtime.stop(spec.coordinator);
    for (const spec of specs) expect(await runtime.containerHealth(spec.coordinator)).toBe("stopped");
    await Promise.all(specs.map((spec) => runtime.startAndVerify(spec, 120_000)));
    const [firstStatus, secondStatus, emailStatus] = await Promise.all(
      specs.map((spec) => runtime.activationStatus(spec.coordinator)),
    );
    expect(firstStatus).toMatchObject({
      activated: true,
      accountId: firstAccount.id,
      coordinatorId: firstCoordinator.id,
      topology: "active-passwordless-google",
    });
    expect(secondStatus).toMatchObject({
      activated: true,
      accountId: secondAccount.id,
      coordinatorId: secondCoordinator.id,
      topology: "active-passwordless-google",
    });
    expect(emailStatus).toMatchObject({
      activated: false,
      accountId: emailAccount.id,
      coordinatorId: emailCoordinator.id,
      topology: "pending-signup-gateway",
    });

    assertGoogleOwnerDatabase(firstCoordinator, firstAccount, GOOGLE_SUBJECT_A);
    assertGoogleOwnerDatabase(secondCoordinator, secondAccount, GOOGLE_SUBJECT_B);
    assertSignupGatewayDatabase(emailCoordinator, emailAccount);

    const central = new Database(registry.path, { readonly: true });
    try {
      expect(central.query(`
        SELECT issuer, subject, account_id, email_normalized, state
        FROM federated_identities
        ORDER BY subject
      `).all()).toEqual([
        {
          issuer: GOOGLE_IDENTITY_ISSUER,
          subject: GOOGLE_SUBJECT_A,
          account_id: firstAccount.id,
          email_normalized: GOOGLE_EMAIL_A,
          state: "active",
        },
        {
          issuer: GOOGLE_IDENTITY_ISSUER,
          subject: GOOGLE_SUBJECT_B,
          account_id: secondAccount.id,
          email_normalized: GOOGLE_EMAIL_B,
          state: "active",
        },
      ]);
    } finally {
      central.close(true);
    }

    await proveCapacityBoundary(root, imageId, initialized.assertionVerifyKeyPath);
    proveLeaseReclaimAndIdempotency(root, imageId);
  } finally {
    if (registry) {
      try {
        for (const coordinator of registry.listCoordinators()) {
          containerNames.add(coordinator.containerName);
        }
      } catch {
        // Cleanup continues using any names captured before the failure.
      }
    }
    for (const containerName of containerNames) dockerCleanup(["rm", "--force", containerName]);
    try { registry?.close(); } catch { /* cleanup */ }
    rmSync(root, { recursive: true, force: true });
  }
}, 420_000);
