// Exercises encrypted SaaS backup staging, process pipelines, and retention.
// Production registry and replay stores provide the live SQLite snapshot fixtures.
// Injected executors isolate archive policy while one focused case owns real child streams.

import { afterEach, describe, expect, test } from "bun:test";
import { Database } from "bun:sqlite";
import {
  chmodSync,
  existsSync,
  mkdtempSync,
  mkdirSync,
  readdirSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { basename, dirname, join } from "node:path";
import { tmpdir } from "node:os";
import { createSqliteSnapshot } from "../../coord/src/db/snapshot.ts";
import { SaasRegistry } from "../src/saas/registry.ts";
import { ProvisionerReplayStore } from "../src/saas-provisioner/replay-store.ts";
import {
  _backupInternals,
  createEncryptedBackup,
  type BackupExecutor,
} from "../src/saas/backup.ts";
import { instanceLayoutFor } from "../src/saas/layout.ts";

const COORDINATOR_ID = "22222222-2222-4222-8222-222222222222";
const ACCOUNT_ID = "11111111-1111-4111-8111-111111111111";
const JOB_ID = "33333333-3333-4333-8333-333333333333";
const PENDING_NONCE = "p".repeat(43);
const COMPLETED_NONCE = "c".repeat(43);
const cleanups: string[] = [];
const openRegistries: SaasRegistry[] = [];
const openReplayStores: ProvisionerReplayStore[] = [];
afterEach(() => {
  while (openReplayStores.length > 0) openReplayStores.pop()!.close();
  while (openRegistries.length > 0) openRegistries.pop()!.close();
  while (cleanups.length > 0) rmSync(cleanups.pop()!, { recursive: true, force: true });
});

function fixture() {
  const root = mkdtempSync(join(tmpdir(), "roost-saas-backup-"));
  cleanups.push(root);
  const registryPath = join(root, "control.db");
  const generatedIds = [ACCOUNT_ID, COORDINATOR_ID, JOB_ID];
  const registry = new SaasRegistry({
    rootDir: root,
    path: registryPath,
    now: () => 1_000,
    createId: () => {
      const id = generatedIds.shift();
      if (!id) throw new Error("backup fixture UUID source exhausted");
      return id;
    },
    createRouteKey: () => "d".repeat(64),
  });
  openRegistries.push(registry);
  const reservation = registry.reserveAccount(
    "backup-owner@example.com",
    `sha256:${"a".repeat(64)}`,
  );
  registry.transitionCoordinator(COORDINATOR_ID, "reserved", "seeded");
  registry.transitionCoordinator(COORDINATOR_ID, "seeded", "running");
  registry.transitionCoordinator(COORDINATOR_ID, "running", "routed");
  registry.transitionCoordinator(COORDINATOR_ID, "routed", "invited");
  const coordinator = registry.markActivationCommitted(ACCOUNT_ID, COORDINATOR_ID);
  registry.acquireLease(COORDINATOR_ID, "backup", "active-backup", 10_000);
  registry.acquireGlobalLease("provisioning-worker", "process", "active-worker", 10_000);
  const insertion = registry.insertProvisioningJob({
    idempotencyKeyHash: "a".repeat(64),
    kind: "verified-email",
    emailNormalized: reservation.account.emailNormalized,
    activationTokenHash: "b".repeat(64),
    verifiedAtMs: 1_000,
    accountId: ACCOUNT_ID,
    coordinatorId: COORDINATOR_ID,
  });
  const [claimed] = registry.claimDueProvisioningJobs({
    leaseDurationMs: 10_000,
    limit: 1,
  });
  if (!claimed || claimed.id !== insertion.job.id) {
    throw new Error("backup fixture did not claim its production provisioning job");
  }

  const replayStore = new ProvisionerReplayStore({
    path: registryPath,
    retentionMs: 60_000,
  });
  openReplayStores.push(replayStore);
  const pendingRequest = Buffer.from("pending-request");
  const completedRequest = Buffer.from("completed-request");
  replayStore.reserve(PENDING_NONCE, pendingRequest, 1_000);
  replayStore.reserve(COMPLETED_NONCE, completedRequest, 1_000);
  replayStore.complete(COMPLETED_NONCE, completedRequest, Buffer.from("response"), 1_000);

  const layout = instanceLayoutFor(coordinator);
  mkdirSync(layout.dataDir, { recursive: true, mode: 0o700 });
  mkdirSync(layout.secretsDir, { recursive: true, mode: 0o700 });
  for (const [path, content] of [
    [layout.authorizedKeysPath, ""],
    [layout.manifestPath, "{}\n"],
    [layout.coordinatorKeyPath, "private-key"],
    [layout.outboxKeyPath, "A".repeat(43)],
    [layout.resendApiKeyPath, "shared-key"],
  ] as const) {
    writeFileSync(path, content, { mode: 0o600 });
    chmodSync(path, 0o600);
  }
  const coordinatorDatabase = new Database(join(layout.dataDir, "coordinator_v2.db"));
  coordinatorDatabase.exec("CREATE TABLE fixture (value TEXT NOT NULL)");
  coordinatorDatabase.close();
  chmodSync(join(layout.dataDir, "coordinator_v2.db"), 0o600);
  return { root, coordinator, layout };
}

function createTestSnapshot(databasePath: string, targetPath: string): Promise<void> {
  const sqlite = new Database(databasePath, { readonly: true });
  try {
    createSqliteSnapshot(sqlite, targetPath);
  } finally {
    sqlite.close(true);
  }
  return Promise.resolve();
}

describe("SaaS encrypted backups", () => {
  test("settles every child stream before returning from the archive pipeline", async () => {
    const result = await _backupInternals.runCommandPipeline(
      [
        process.execPath,
        "-e",
        'process.stdout.write("archive-bytes"); process.stderr.write("producer-note")',
      ],
      [
        process.execPath,
        "-e",
        'const input = await Bun.stdin.text(); process.stdout.write(input.toUpperCase()); process.stderr.write("consumer-note")',
      ],
    );
    expect(result).toEqual({
      producerExitCode: 0,
      consumerExitCode: 0,
      consumerStdout: "ARCHIVE-BYTES",
      producerStderr: "producer-note",
      consumerStderr: "consumer-note",
    });
  });

  test("streams only required instance files through age and verifies contents", async () => {
    const opened = fixture();
    const calls: {
      tar?: readonly string[];
      age?: readonly string[];
      snapshotSources: string[];
      leases?: number;
      runningJobs?: number;
      pendingJobs?: number;
      accounts?: number;
      schemaVersion?: number;
      pendingReplays?: number;
      completedReplays?: number;
    } = { snapshotSources: [] };
    const executor: BackupExecutor = {
      snapshot: async (databasePath, targetPath) => {
        calls.snapshotSources.push(databasePath);
        await createTestSnapshot(databasePath, targetPath);
      },
      encrypt: async (tarArgv, ageArgv, destination) => {
        calls.tar = tarArgv;
        calls.age = ageArgv;
        const stagingDirectory = tarArgv[tarArgv.indexOf("-C") + 1];
        if (!stagingDirectory) throw new Error("backup tar staging directory was absent");
        const registry = new Database(join(stagingDirectory, "control.db"), { readonly: true });
        try {
          const leaseCount = registry.query<{ count: number }, []>(
            "SELECT (SELECT count(*) FROM operation_leases) + (SELECT count(*) FROM global_leases) AS count",
          ).get();
          const runningJobCount = registry.query<{ count: number }, []>(
            "SELECT count(*) AS count FROM provisioning_jobs WHERE state = 'running'",
          ).get();
          calls.leases = leaseCount?.count;
          calls.runningJobs = runningJobCount?.count;
          const pendingJobCount = registry.query<{ count: number }, []>(`
            SELECT count(*) AS count
            FROM provisioning_jobs
            WHERE state = 'pending' AND locked_until_ms IS NULL AND lease_token IS NULL
          `).get();
          const accountCount = registry.query<{ count: number }, []>(
            "SELECT count(*) AS count FROM accounts",
          ).get();
          const schemaVersion = registry.query<{ user_version: number }, []>(
            "PRAGMA user_version",
          ).get();
          calls.pendingJobs = pendingJobCount?.count;
          calls.accounts = accountCount?.count;
          calls.schemaVersion = schemaVersion?.user_version;
          const pendingReplayCount = registry.query<{ count: number }, []>(
            "SELECT count(*) AS count FROM provisioner_ipc_replays WHERE response_bytes IS NULL",
          ).get();
          const completedReplayCount = registry.query<{ count: number }, []>(
            "SELECT count(*) AS count FROM provisioner_ipc_replays WHERE response_bytes IS NOT NULL",
          ).get();
          calls.pendingReplays = pendingReplayCount?.count;
          calls.completedReplays = completedReplayCount?.count;
        } finally {
          registry.close();
        }
        writeFileSync(destination, "encrypted", { mode: 0o600 });
      },
      verify: async () => [
        "control.db",
        "coordinator.db",
        "authorized_keys.roost",
        "instance.json",
        "ssh_ed25519.key",
        "email-outbox-key",
      ],
    };
    const destination = await createEncryptedBackup(opened.coordinator, {
      rootDir: opened.root,
      ageRecipient: "age1testrecipientxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx",
      ageIdentityFile: join(opened.root, "age-key.txt"),
      executor,
      now: () => new Date("2026-08-30T12:34:56.000Z"),
    });
    expect(destination).toEndWith("2026-08-30T12-34-56-000Z.tar.age");
    expect(calls.tar?.[0]).toBe("tar");
    expect(calls.age?.slice(0, 4)).toEqual([
      "age", "--recipient", "age1testrecipientxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx", "--output",
    ]);
    expect(calls.age?.at(-1)).toBe(join(dirname(destination), `.${basename(destination)}.tmp`));
    expect(calls.snapshotSources).toEqual([
      join(opened.layout.dataDir, "coordinator_v2.db"),
      join(opened.root, "control.db"),
    ]);
    const argv = calls.tar?.join(" ") ?? "";
    expect(argv).toContain("ssh_ed25519.key");
    expect(argv).toContain("email-outbox-key");
    expect(argv).not.toContain("resend-api-key");
    expect(argv).toContain("control.db");
    expect(calls.leases).toBe(0);
    expect(calls.runningJobs).toBe(0);
    expect(calls.pendingJobs).toBe(1);
    expect(calls.accounts).toBe(1);
    expect(calls.schemaVersion).toBe(3);
    expect(calls.pendingReplays).toBe(0);
    expect(calls.completedReplays).toBe(1);
    expect(existsSync(destination)).toBe(true);
    expect(readdirSync(join(opened.root, "backups", COORDINATOR_ID)).some((name) => name.includes("snapshot"))).toBe(false);
  });

  test("removes unverifiable output and retains only fourteen daily artifacts", async () => {
    const opened = fixture();
    const backupDir = join(opened.root, "backups", COORDINATOR_ID);
    mkdirSync(backupDir, { recursive: true, mode: 0o700 });
    for (let day = 1; day <= 14; day++) {
      writeFileSync(join(backupDir, `2026-08-${String(day).padStart(2, "0")}T00-00-00-000Z.tar.age`), "old");
    }
    const goodExecutor: BackupExecutor = {
      snapshot: createTestSnapshot,
      encrypt: async (_tar, _age, destination) => { writeFileSync(destination, "encrypted", { mode: 0o600 }); },
      verify: async () => [
        "control.db", "coordinator.db", "authorized_keys.roost", "instance.json",
        "ssh_ed25519.key", "email-outbox-key",
      ],
    };
    await createEncryptedBackup(opened.coordinator, {
      rootDir: opened.root,
      ageRecipient: "age1testrecipientxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx",
      ageIdentityFile: join(opened.root, "age-key.txt"),
      executor: goodExecutor,
      now: () => new Date("2026-08-30T00:00:00.000Z"),
    });
    expect(readdirSync(backupDir).filter((name) => name.endsWith(".tar.age"))).toHaveLength(14);
    expect(existsSync(join(backupDir, "2026-08-01T00-00-00-000Z.tar.age"))).toBe(false);

    const badExecutor: BackupExecutor = {
      ...goodExecutor,
      verify: async () => ["control.db", "coordinator.db"],
    };
    await expect(createEncryptedBackup(opened.coordinator, {
      rootDir: opened.root,
      ageRecipient: "age1testrecipientxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx",
      ageIdentityFile: join(opened.root, "age-key.txt"),
      executor: badExecutor,
      now: () => new Date("2026-09-01T00:00:00.000Z"),
    })).rejects.toThrow("missing authorized_keys.roost");
    expect(existsSync(join(backupDir, "2026-09-01T00-00-00-000Z.tar.age"))).toBe(false);
  });
});
