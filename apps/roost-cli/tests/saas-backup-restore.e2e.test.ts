// This E2E proves a managed coordinator backup can be decrypted, restored, and booted.
// The managed profile supplies its immutable image and shared Docker network.
// The scenario owns only its coordinator container, backup data, and restore directories.

import { test, expect } from "bun:test";
import {
  chmodSync,
  chownSync,
  copyFileSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  statSync,
  writeFileSync,
} from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { _backupInternals, createEncryptedBackup } from "../src/saas/backup.ts";
import { ManagedInstanceRuntime, type ManagedInstanceSpec } from "../src/saas/docker.ts";
import { SaasLifecycle, type TenantRouteManager } from "../src/saas/lifecycle.ts";
import { SaasRegistry, type RegistryAccount, type RegistryCoordinator } from "../src/saas/registry.ts";
import { ProvisionerReplayStore } from "../src/saas-provisioner/replay-store.ts";
import { instanceLayoutFor } from "../src/saas/layout.ts";
import {
  requiredManagedE2eResources,
  writeEd25519VerificationKeyFixture,
} from "./managed-e2e-fixture.ts";

const enabled = process.env.ROOST_SAAS_E2E === "1";
const ACCOUNT_ID = "55555555-5555-4555-8555-555555555555";
const COORDINATOR_ID = "66666666-6666-4666-8666-666666666666";
const JOB_ID = "77777777-7777-4777-8777-777777777777";
const PENDING_NONCE = "p".repeat(43);
const COMPLETED_NONCE = "c".repeat(43);
const ROUTE_KEY = "b".repeat(64);


async function extractBackup(backup: string, identity: string, destination: string): Promise<void> {
  const result = await _backupInternals.runCommandPipeline(
    ["age", "--decrypt", "--identity", identity, backup],
    ["tar", "-xf", "-", "-C", destination],
  );
  if (result.producerExitCode !== 0 || result.consumerExitCode !== 0) {
    throw new Error("backup extraction failed");
  }
}

function makeSpec(
  account: RegistryAccount,
  coordinator: RegistryCoordinator,
  sharedKey: string,
  authVerifyKeyFile: string,
): ManagedInstanceSpec {
  return {
    account,
    coordinator,
    authVerifyKeyFile,
    email: {
      resendEndpoint: "https://api.resend.com/emails",
      emailFrom: "Roost Restore <noreply@example.com>",
      sharedResendApiKeyPath: sharedKey,
    },
  };
}

test.skipIf(!enabled)("restores coordinated registry, replay, and tenant state before reconciliation", async () => {
  if (typeof process.getuid !== "function" || process.getuid() !== 0) {
    throw new Error("ROOST_SAAS_E2E requires Linux root");
  }
  const { imageId, network } = requiredManagedE2eResources();
  const root = mkdtempSync(join(tmpdir(), "roost-saas-backup-e2e-"));
  const extractionRoot = mkdtempSync(join(tmpdir(), "roost-saas-restore-e2e-"));
  const extractDir = join(extractionRoot, "extracted");
  mkdirSync(extractDir, { mode: 0o700 });
  const sharedKey = join(root, "resend-key");
  writeFileSync(sharedKey, "re_restore_not_delivered", { mode: 0o600 });
  const authVerifyKeyFile = join(root, "saas-auth-verify-key");
  writeEd25519VerificationKeyFixture(authVerifyKeyFile);
  const registryPath = join(root, "control.db");
  const generatedIds = [ACCOUNT_ID, COORDINATOR_ID, JOB_ID];
  let registry: SaasRegistry | null = new SaasRegistry({
    rootDir: root,
    path: registryPath,
    now: () => 1_000,
    createId: () => {
      const id = generatedIds.shift();
      if (!id) throw new Error("backup restore UUID source exhausted");
      return id;
    },
    createRouteKey: () => ROUTE_KEY,
  });
  const reservation = registry.reserveAccount("restore@example.com", imageId);
  const seededCoordinator = registry.transitionCoordinator(
    reservation.coordinator.id,
    "reserved",
    "seeded",
  );
  const spec = makeSpec(reservation.account, seededCoordinator, sharedKey, authVerifyKeyFile);
  const insertion = registry.insertProvisioningJob({
    idempotencyKeyHash: "a".repeat(64),
    kind: "verified-email",
    emailNormalized: reservation.account.emailNormalized,
    activationTokenHash: "b".repeat(64),
    verifiedAtMs: 1_000,
    accountId: reservation.account.id,
    coordinatorId: seededCoordinator.id,
  });
  registry.claimDueProvisioningJobs({ leaseDurationMs: 10_000, limit: 1 });
  registry.acquireLease(COORDINATOR_ID, "backup", "source-backup", 10_000);
  registry.acquireGlobalLease("provisioning-worker", "process", "source-worker", 10_000);
  const pendingRequest = Buffer.from("pending-request");
  const completedRequest = Buffer.from("completed-request");
  let replayStore: ProvisionerReplayStore | null = new ProvisionerReplayStore({
    path: registryPath,
    retentionMs: 60_000,
  });
  replayStore.reserve(PENDING_NONCE, pendingRequest, 1_000);
  replayStore.reserve(COMPLETED_NONCE, completedRequest, 1_000);
  replayStore.complete(COMPLETED_NONCE, completedRequest, Buffer.from("response"), 1_000);

  const runtime = new ManagedInstanceRuntime({ network });
  const closeRestoreStores = (failures: unknown[]): void => {
    const ownedReplayStore = replayStore;
    replayStore = null;
    try { ownedReplayStore?.close(); } catch (error) { failures.push(error); }
    const ownedRegistry = registry;
    registry = null;
    try { ownedRegistry?.close(); } catch (error) { failures.push(error); }
  };
  const removeRestoreContainer = async (failures: unknown[]): Promise<void> => {
    try { await runtime.stop(spec.coordinator); } catch (error) { failures.push(error); }
    try {
      await runtime.removeContainer(spec.coordinator.containerName);
    } catch (error) {
      failures.push(error);
    }
  };
  const assertCleanupSucceeded = (failures: unknown[]): void => {
    if (failures.length > 0) throw new AggregateError(failures, "restore cleanup failed");
  };
  try {
    await runtime.seedOwnerActivation(spec);
    await runtime.startAndVerify(spec);

    const ageIdentity = join(root, "age-identity.txt");
    const keygen = Bun.spawnSync(["age-keygen", "--output", ageIdentity], {
      stdout: "pipe",
      stderr: "pipe",
    });
    if (keygen.exitCode !== 0) throw new Error("age identity generation failed");
    const recipientResult = Bun.spawnSync(["age-keygen", "--y", ageIdentity], {
      stdout: "pipe",
      stderr: "pipe",
    });
    if (recipientResult.exitCode !== 0) throw new Error("age recipient derivation failed");
    const backup = await createEncryptedBackup(spec.coordinator, {
      rootDir: root,
      ageRecipient: recipientResult.stdout.toString().trim(),
      ageIdentityFile: ageIdentity,
    });
    expect(statSync(backup).mode & 0o777).toBe(0o600);
    expect(readFileSync(backup).includes(Buffer.from("owner_activation_tokens"))).toBe(false);

    const closedHandleFailures: unknown[] = [];
    await removeRestoreContainer(closedHandleFailures);
    closeRestoreStores(closedHandleFailures);
    assertCleanupSucceeded(closedHandleFailures);
    expect(await runtime.containerHealth(spec.coordinator)).toBe("missing");
    await extractBackup(backup, ageIdentity, extractDir);

    const layout = instanceLayoutFor(spec.coordinator);
    rmSync(layout.instanceDir, { recursive: true, force: true });
    for (const suffix of ["", "-wal", "-shm"]) rmSync(`${registryPath}${suffix}`, { force: true });
    mkdirSync(layout.dataDir, { recursive: true, mode: 0o700 });
    mkdirSync(layout.secretsDir, { recursive: true, mode: 0o700 });
    for (const directory of [layout.instanceDir, layout.dataDir, layout.secretsDir]) {
      chmodSync(directory, 0o700);
      chownSync(directory, 65_532, 65_532);
    }
    for (const [source, destination] of [
      [join(extractDir, "coordinator.db"), join(layout.dataDir, "coordinator_v2.db")],
      [join(extractDir, "authorized_keys.roost"), layout.authorizedKeysPath],
      [join(extractDir, "instance.json"), layout.manifestPath],
      [join(extractDir, "ssh_ed25519.key"), layout.coordinatorKeyPath],
      [join(extractDir, "email-outbox-key"), layout.outboxKeyPath],
    ] as const) {
      copyFileSync(source, destination);
      chmodSync(destination, 0o600);
      chownSync(destination, 65_532, 65_532);
    }
    copyFileSync(sharedKey, layout.resendApiKeyPath);
    chmodSync(layout.resendApiKeyPath, 0o600);
    chownSync(layout.resendApiKeyPath, 65_532, 65_532);
    copyFileSync(join(extractDir, "control.db"), registryPath);
    chmodSync(registryPath, 0o600);

    registry = new SaasRegistry({ rootDir: root, path: registryPath, now: () => 2_000 });
    expect(registry.getAccount(ACCOUNT_ID).routeKey).toBe(ROUTE_KEY);
    expect(registry.getCoordinator(COORDINATOR_ID).state).toBe("seeded");
    expect(registry.getLease(COORDINATOR_ID)).toBeNull();
    expect(registry.getGlobalLease("provisioning-worker")).toBeNull();
    expect(registry.getProvisioningJob(insertion.job.id)).toEqual(expect.objectContaining({
      state: "pending",
      lockedUntilMs: null,
      leaseToken: null,
    }));
    replayStore = new ProvisionerReplayStore({ path: registryPath, retentionMs: 60_000 });
    expect(replayStore.reserve(COMPLETED_NONCE, completedRequest, 2_000))
      .toEqual({ state: "replay", response: Buffer.from("response") });
    expect(replayStore.reserve(PENDING_NONCE, pendingRequest, 2_000)).toEqual({ state: "new" });

    const routeSnapshots: string[][] = [];
    const routes: TenantRouteManager = {
      reconcile: async (coordinators) => {
        routeSnapshots.push(coordinators.map((coordinator) => coordinator.id));
      },
      verify: async () => {},
      verifyResolver: async () => {},
    };
    const lifecycle = new SaasLifecycle({
      registry,
      runtime,
      routes,
      admission: {
        assertBeforeReservation: async (reserve) => reserve(),
        assertPendingWorkAllowed: async () => {},
      },
      email: spec.email,
      authVerifyKeyFile,
      now: () => 2_000,
    });
    expect(await lifecycle.reconcile()).toEqual([{
      coordinatorId: COORDINATOR_ID,
      state: "invited",
      repaired: true,
    }]);
    expect(routeSnapshots.at(-1)).toEqual([COORDINATOR_ID]);
    const restoredCoordinator = registry.getCoordinator(COORDINATOR_ID);
    const status = await runtime.activationStatus(restoredCoordinator);
    expect(status).toEqual(expect.objectContaining({
      activated: false,
      accountId: ACCOUNT_ID,
      coordinatorId: COORDINATOR_ID,
    }));
  } finally {
    const cleanupFailures: unknown[] = [];
    closeRestoreStores(cleanupFailures);
    await removeRestoreContainer(cleanupFailures);
    for (const path of [root, extractionRoot]) {
      try { rmSync(path, { recursive: true, force: true }); }
      catch (error) { cleanupFailures.push(error); }
    }
    assertCleanupSucceeded(cleanupFailures);
  }
}, 180_000);
