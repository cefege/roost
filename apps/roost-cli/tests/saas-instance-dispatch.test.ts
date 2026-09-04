/**
 * This suite separates managed CLI admission from portable instance runtime behavior.
 * Direct subprocesses keep repeated actions and output redaction isolated on every platform.
 * Shared database readers let the test inspect state without duplicating secret handling.
 */
import { Database } from "bun:sqlite";
import { afterEach, expect, test } from "bun:test";
import { existsSync, readFileSync } from "node:fs";
import { mkdtemp, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import {
  ACCOUNT_ID,
  COORDINATOR_ID,
  EMAIL,
  OUTBOX_KEY,
  PUBLIC_URL,
  ROOT,
  ROUTE_KEY,
  cleanupSaasInstanceRoots,
  decryptedToken,
  outbox,
} from "./saas-instance-fixtures.ts";
import { writeEd25519VerificationKeyFixture } from "./managed-e2e-fixture.ts";

const roots: string[] = [];
const EMPTY_ENV_FILE = process.platform === "win32" ? "NUL" : "/dev/null";
const INSTANCE_RUNNER_SOURCE = `
  import { saasInstance } from "./apps/roost-cli/src/saas-instance.ts";
  await saasInstance(process.argv.slice(1));
`;

afterEach(async () => {
  await cleanupSaasInstanceRoots(roots);
});

test("main refuses an unmanaged hidden-instance dispatch before runtime setup", async () => {
  const root = await mkdtemp(join(tmpdir(), "roost-saas-instance-refused-"));
  roots.push(root);
  const databasePath = join(root, "coordinator_v2.db");
  const result = Bun.spawnSync([
    process.execPath,
    `--env-file=${EMPTY_ENV_FILE}`,
    "apps/roost-cli/src/main.ts",
    "__saas-instance",
    "health",
  ], {
    cwd: ROOT,
    env: {
      PATH: process.env.PATH ?? "/usr/bin:/bin",
      HOME: root,
      ROOST_COORD_DATA_DIR: root,
      ROOST_COORDINATOR_DB: databasePath,
    },
  });
  const output = result.stdout.toString() + result.stderr.toString();
  expect(result.exitCode).not.toBe(0);
  expect(output).toContain("SaaS instance entry is not admitted on this host");
  expect(existsSync(databasePath)).toBe(false);
});

test("instance actions use configured mounts repeatedly without emitting secrets", async () => {
  const root = await mkdtemp(join(tmpdir(), "roost-saas-instance-dispatch-"));
  roots.push(root);
  const databasePath = join(root, "coordinator_v2.db");
  const keyPath = join(root, "coordinator.key");
  const resendSecretPath = join(root, "resend-api-key");
  const outboxSecretPath = join(root, "email-outbox-key");
  const authVerifyKeyPath = join(root, "saas-auth-verify-key");
  writeEd25519VerificationKeyFixture(authVerifyKeyPath);
  await Promise.all([
    writeFile(resendSecretPath, "test-resend-api-key", { mode: 0o600 }),
    writeFile(outboxSecretPath, OUTBOX_KEY, { mode: 0o600 }),
  ]);
  const args = [
    process.execPath,
    `--env-file=${EMPTY_ENV_FILE}`,
    "-e",
    INSTANCE_RUNNER_SOURCE,
    "--",
    "seed-owner-activation",
    "--account-id",
    ACCOUNT_ID,
    "--coordinator-id",
    COORDINATOR_ID,
    "--email",
    EMAIL,
  ];
  const environment: Record<string, string> = {
    PATH: process.env.PATH ?? "/usr/bin:/bin",
    HOME: root,
    ROOST_COORD_DATA_DIR: root,
    ROOST_COORDINATOR_DB: databasePath,
    ROOST_COORDINATOR_KEY_PATH: keyPath,
    ROOST_COORDINATOR_AUTHORIZED_KEYS: join(root, "authorized_keys.roost"),
    ROOST_COORDINATOR_LOG_DIR: join(root, "logs"),
    ROOST_COORDINATOR_BIND: "127.0.0.1:4103",
    ROOST_PUBLIC_BIND: "0.0.0.0:4104",
    ROOST_TRUST_PROXY: "1",
    ROOST_MANAGED_CONTAINER: "1",
    ROOST_COORDINATOR_INSTANCE_ID: COORDINATOR_ID,
    ROOST_SAAS_MODE: "1",
    ROOST_WEB_PUBLIC_URL: PUBLIC_URL,
    ROOST_TENANT_ROUTE_KEY: ROUTE_KEY,
    ROOST_RESEND_ENDPOINT: "https://api.resend.com/emails",
    ROOST_RESEND_API_KEY_FILE: resendSecretPath,
    ROOST_EMAIL_FROM: "Roost <noreply@example.com>",
    ROOST_EMAIL_OUTBOX_KEY_FILE: outboxSecretPath,
    ROOST_SAAS_AUTH_VERIFY_KEY_FILE: authVerifyKeyPath,
  };
  const internalPrefix = args.slice(0, 5);
  const unreadyHealth = Bun.spawnSync([...internalPrefix, "health"], {
    cwd: ROOT,
    env: environment,
  });
  const unreadyHealthOutput =
    unreadyHealth.stdout.toString() + unreadyHealth.stderr.toString();
  expect(unreadyHealth.exitCode).not.toBe(0);
  expect(existsSync(databasePath)).toBe(false);
  for (const forbidden of [
    EMAIL,
    OUTBOX_KEY,
    "test-resend-api-key",
    resendSecretPath,
    outboxSecretPath,
  ]) {
    expect(unreadyHealthOutput).not.toContain(forbidden);
  }
  const first = Bun.spawnSync(args, { cwd: ROOT, env: environment });
  const firstOutput = first.stdout.toString() + first.stderr.toString();
  expect(first.exitCode, firstOutput).toBe(0);
  expect(existsSync(databasePath)).toBe(true);
  expect(existsSync(keyPath)).toBe(true);

  const sqlite = new Database(databasePath, { readonly: true });
  let secret: { link: string; token: string };
  let encryptedPayload: string;
  try {
    const row = outbox(sqlite);
    secret = decryptedToken(row);
    encryptedPayload = row.encrypted_payload;
  } finally {
    sqlite.close(true);
  }
  const privateKey = readFileSync(keyPath, "utf8");
  expect(firstOutput).toContain("saas_instance.owner_activation_seeded");
  expect(firstOutput).toContain(ACCOUNT_ID);
  expect(firstOutput).toContain(COORDINATOR_ID);
  for (const forbidden of [
    EMAIL,
    secret.token,
    secret.link,
    encryptedPayload,
    OUTBOX_KEY,
    privateKey.trim(),
    "test-resend-api-key",
    keyPath,
    "coord-key",
  ]) {
    expect(firstOutput).not.toContain(forbidden);
  }

  const second = Bun.spawnSync(args, { cwd: ROOT, env: environment });
  const secondOutput = second.stdout.toString() + second.stderr.toString();
  expect(second.exitCode, secondOutput).toBe(0);
  expect(readFileSync(keyPath, "utf8")).toBe(privateKey);

  const reseededSqlite = new Database(databasePath, { readonly: true });
  let currentSecret: { link: string; token: string };
  let currentEncryptedPayload: string;
  try {
    const row = outbox(reseededSqlite);
    currentSecret = decryptedToken(row);
    currentEncryptedPayload = row.encrypted_payload;
    expect(row.next_attempt_ms).toBe(Number.MAX_SAFE_INTEGER);
  } finally {
    reseededSqlite.close(true);
  }
  expect(secondOutput).not.toContain(currentSecret.token);
  expect(secondOutput).not.toContain(currentSecret.link);

  const statusOutputs: string[] = [];
  for (let attempt = 0; attempt < 2; attempt += 1) {
    const status = Bun.spawnSync([...internalPrefix, "activation-status"], {
      cwd: ROOT,
      env: environment,
    });
    const statusOutput = status.stdout.toString() + status.stderr.toString();
    expect(status.exitCode, statusOutput).toBe(0);
    expect(statusOutput).toContain("\"activated\":false");
    statusOutputs.push(statusOutput);
  }

  const releaseOutputs: string[] = [];
  for (let attempt = 0; attempt < 2; attempt += 1) {
    const release = Bun.spawnSync([...internalPrefix, "release-owner-activation-email"], {
      cwd: ROOT,
      env: environment,
    });
    const releaseOutput = release.stdout.toString() + release.stderr.toString();
    expect(release.exitCode, releaseOutput).toBe(0);
    expect(releaseOutput).toContain("saas_instance.owner_activation_email_released");
    releaseOutputs.push(releaseOutput);
  }
  const releasedSqlite = new Database(databasePath, { readonly: true });
  try {
    expect(outbox(releasedSqlite).next_attempt_ms).toBeLessThan(Number.MAX_SAFE_INTEGER);
  } finally {
    releasedSqlite.close(true);
  }

  for (const output of [secondOutput, ...statusOutputs, ...releaseOutputs]) {
    for (const forbidden of [
      EMAIL,
      currentSecret.token,
      currentSecret.link,
      currentEncryptedPayload,
      OUTBOX_KEY,
      privateKey.trim(),
      "test-resend-api-key",
      keyPath,
      "coord-key",
    ]) {
      expect(output).not.toContain(forbidden);
    }
  }
}, 30_000);
