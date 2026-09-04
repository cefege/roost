/**
 * This fixture owns registry setup and assertions for the open-signup E2E flow.
 * The managed profile supplies the immutable image and shared Docker network.
 * It depends on the production SaaS host, registry, and provisioning worker APIs.
 */

import { expect } from "bun:test";
import { Database } from "bun:sqlite";
import { join } from "node:path";
import { HostAdmission, HostCapacityError, type SaasHostConfig } from "../src/saas/host.ts";
import {
  GOOGLE_IDENTITY_ISSUER,
  SaasRegistry,
  type RegistryAccount,
  type RegistryCoordinator,
} from "../src/saas/registry.ts";
import {
  ManagedInstanceRuntime,
  type ManagedInstanceSpec,
} from "../src/saas/docker.ts";
import {
  ProvisioningWorker,
  type ProvisioningSubmitResult,
} from "../src/saas/provisioner-worker.ts";
import { instanceLayoutFor } from "../src/saas/layout.ts";

const enabled = process.env.ROOST_SIGNUP_E2E === "1";
const GOOGLE_SUBJECT_A = "signup-e2e-google-subject-a";
const GOOGLE_SUBJECT_B = "signup-e2e-google-subject-b";
const GOOGLE_EMAIL_A = "google-a@signup-e2e.example";
const GOOGLE_EMAIL_B = "google-b@signup-e2e.example";
const EMAIL_OWNER = "email-owner@signup-e2e.example";
const ACTIVATION_HASH = "a".repeat(64);

class ManualProvisioningWorker extends ProvisioningWorker {
  override kick(): void {
    // The production worker is deliberately driven one claim at a time here so
    // the test can observe its concurrency-one and lease boundaries exactly.
  }
}

function docker(args: readonly string[]): string {
  const result = Bun.spawnSync(["docker", ...args], {
    stdout: "pipe",
    stderr: "pipe",
  });
  if (result.exitCode !== 0) {
    throw new Error(`docker ${args[0] ?? "command"} failed: ${result.stderr.toString().trim()}`);
  }
  return result.stdout.toString().trim();
}

function dockerCleanup(args: readonly string[]): void {
  Bun.spawnSync(["docker", ...args], {
    stdout: "ignore",
    stderr: "ignore",
  });
}

function pending(result: ProvisioningSubmitResult): Extract<ProvisioningSubmitResult, { state: "pending" }> {
  if (result.state !== "pending") throw new Error(`expected pending provisioning result, got ${result.state}`);
  return result;
}

function instanceSpec(
  registry: SaasRegistry,
  accountId: string,
  coordinatorId: string,
  authVerifyKeyFile: string,
  sharedResendApiKeyPath: string,
): ManagedInstanceSpec {
  return {
    account: registry.getAccount(accountId),
    coordinator: registry.getCoordinator(coordinatorId),
    authVerifyKeyFile,
    email: {
      resendEndpoint: "https://api.resend.com/emails",
      emailFrom: "Roost signup E2E <noreply@signup-e2e.example>",
      sharedResendApiKeyPath,
    },
  };
}

function coordinatorDatabase(coordinator: RegistryCoordinator): Database {
  return new Database(join(instanceLayoutFor(coordinator).dataDir, "coordinator_v2.db"), {
    readonly: true,
  });
}

function count(sqlite: Database, table: string): number {
  const row = sqlite.query<{ count: number }, []>(
    `SELECT COUNT(*) AS count FROM ${table}`,
  ).get();
  if (!row) throw new Error(`could not count coordinator table ${table}`);
  return row.count;
}

function assertGoogleOwnerDatabase(
  coordinator: RegistryCoordinator,
  account: RegistryAccount,
  subject: string,
): void {
  const sqlite = coordinatorDatabase(coordinator);
  try {
    expect(sqlite.query(`
      SELECT id, email_normalized, password_hash, password_changed_at_ms, status
      FROM accounts
    `).all()).toEqual([{
      id: account.id,
      email_normalized: account.emailNormalized,
      password_hash: null,
      password_changed_at_ms: null,
      status: "active",
    }]);
    expect(sqlite.query(`
      SELECT account_id, issuer, subject, email_normalized, revoked_at_ms
      FROM account_identities
    `).all()).toEqual([{
      account_id: account.id,
      issuer: GOOGLE_IDENTITY_ISSUER,
      subject,
      email_normalized: account.emailNormalized,
      revoked_at_ms: null,
    }]);
    expect(count(sqlite, "owner_activation_tokens")).toBe(0);
    expect(count(sqlite, "account_devices")).toBe(0);
    expect(count(sqlite, "authorized_keys")).toBe(0);
    expect(count(sqlite, "email_outbox")).toBe(0);
  } finally {
    sqlite.close(true);
  }
}

function assertSignupGatewayDatabase(
  coordinator: RegistryCoordinator,
  account: RegistryAccount,
): void {
  const sqlite = coordinatorDatabase(coordinator);
  try {
    expect(count(sqlite, "accounts")).toBe(0);
    expect(count(sqlite, "account_identities")).toBe(0);
    expect(count(sqlite, "account_devices")).toBe(0);
    expect(count(sqlite, "authorized_keys")).toBe(0);
    expect(count(sqlite, "email_outbox")).toBe(0);
    expect(sqlite.query(`
      SELECT coordinator_id, account_id, email_normalized, token_hash,
             outbox_id, delivery, accepted_at_ms, revoked_at_ms
      FROM owner_activation_tokens
    `).all()).toEqual([{
      coordinator_id: coordinator.id,
      account_id: account.id,
      email_normalized: account.emailNormalized,
      token_hash: ACTIVATION_HASH,
      outbox_id: null,
      delivery: "signup-gateway",
      accepted_at_ms: null,
      revoked_at_ms: null,
    }]);
  } finally {
    sqlite.close(true);
  }
}

function deterministicIds() {
  let id = 0;
  let route = 0;
  return {
    createId: () => {
      id += 1;
      return `00000000-0000-4000-8000-${id.toString(16).padStart(12, "0")}`;
    },
    createRouteKey: () => {
      route += 1;
      return route.toString(16).padStart(64, "0");
    },
  };
}

async function proveCapacityBoundary(root: string, imageDigest: string, authVerifyKeyFile: string): Promise<void> {
  const fixtureRoot = join(root, "capacity");
  const ids = deterministicIds();
  const registry = new SaasRegistry({
    rootDir: fixtureRoot,
    path: join(fixtureRoot, "control.db"),
    now: () => 10_000,
    ...ids,
  });
  const config: SaasHostConfig = {
    rootDir: fixtureRoot,
    registryPath: registry.path,
    maxAccounts: 8,
    imageDigest,
    network: "unused-capacity-network",
    resendEndpoint: "https://api.resend.com/emails",
    emailFrom: "Roost signup E2E <noreply@signup-e2e.example>",
    sharedResendApiKeyPath: join(root, "resend-api-key"),
    authVerifyKeyFile,
    ageRecipient: "age1signupfixture",
    ageIdentityFile: join(root, "unused-age-identity"),
    caddyConfDir: join(root, "unused-caddy-conf"),
    caddyfilePath: join(root, "unused-Caddyfile"),
    cloudflaredConfigPath: join(root, "unused-cloudflared.yml"),
    caddyImageDigest: imageDigest,
  };
  const admission = new HostAdmission({
    registry,
    config,
    diskRatio: () => 0.1,
    now: () => 10_000,
  });
  try {
    for (let index = 1; index <= 8; index += 1) {
      const reservation = await admission.assertBeforeReservation(() =>
        registry.reserveAccount(`capacity-${index}@signup-e2e.example`, imageDigest)
      );
      expect(reservation.resumed).toBe(false);
    }
    let ninthCallbackRan = false;
    await expect(admission.assertBeforeReservation(() => {
      ninthCallbackRan = true;
      return registry.reserveAccount("capacity-9@signup-e2e.example", imageDigest);
    })).rejects.toBeInstanceOf(HostCapacityError);
    expect(ninthCallbackRan).toBe(false);
    expect(registry.listAccounts()).toHaveLength(8);
    expect(registry.getAccountByEmail("capacity-9@signup-e2e.example")).toBeNull();
  } finally {
    registry.close();
  }
}

function proveLeaseReclaimAndIdempotency(root: string, imageDigest: string): void {
  const fixtureRoot = join(root, "lease-reclaim");
  const now = { value: 20_000 };
  const ids = deterministicIds();
  const registry = new SaasRegistry({
    rootDir: fixtureRoot,
    path: join(fixtureRoot, "control.db"),
    now: () => now.value,
    ...ids,
  });
  try {
    const firstReservation = registry.reserveAccount("lease@signup-e2e.example", imageDigest);
    const retriedReservation = registry.reserveAccount("lease@signup-e2e.example", imageDigest);
    expect(retriedReservation.resumed).toBe(true);
    expect(retriedReservation.account.id).toBe(firstReservation.account.id);
    expect(retriedReservation.coordinator.id).toBe(firstReservation.coordinator.id);
    expect(retriedReservation.account.routeKey).toBe(firstReservation.account.routeKey);

    const options = {
      idempotencyKeyHash: "b".repeat(64),
      kind: "verified-email" as const,
      emailNormalized: firstReservation.account.emailNormalized,
      activationTokenHash: "c".repeat(64),
      verifiedAtMs: now.value,
      accountId: firstReservation.account.id,
      coordinatorId: firstReservation.coordinator.id,
    };
    const inserted = registry.insertProvisioningJob(options);
    const retried = registry.insertProvisioningJob(options);
    expect(inserted.inserted).toBe(true);
    expect(retried.inserted).toBe(false);
    expect(retried.job.id).toBe(inserted.job.id);

    const [firstClaim] = registry.claimDueProvisioningJobs({ leaseDurationMs: 100, limit: 1 });
    if (!firstClaim) throw new Error("provisioning lease fixture was not claimed");
    expect(firstClaim.attempts).toBe(1);
    now.value += 101;
    const [reclaimed] = registry.claimDueProvisioningJobs({ leaseDurationMs: 100, limit: 1 });
    if (!reclaimed) throw new Error("expired provisioning lease was not reclaimed");
    expect(reclaimed.id).toBe(firstClaim.id);
    expect(reclaimed.attempts).toBe(2);
    expect(reclaimed.leaseToken).not.toBe(firstClaim.leaseToken);
    expect(registry.markProvisioningJobSucceeded(firstClaim.id, firstClaim.leaseToken)).toBe(false);
    expect(registry.markProvisioningJobSucceeded(reclaimed.id, reclaimed.leaseToken)).toBe(true);
    expect(registry.getProvisioningJob(reclaimed.id)).toMatchObject({
      state: "succeeded",
      attempts: 2,
    });
  } finally {
    registry.close();
  }
}

export {
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
};
