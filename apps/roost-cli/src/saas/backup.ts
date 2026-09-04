// Creates encrypted, crash-safe snapshots of SaaS registry and tenant data.
// Operator backup commands call this module before retention or restore workflows.
// Descriptor checks and atomic renames keep secrets and SQLite snapshots consistent.
import { Database } from "bun:sqlite";
import {
  chmodSync,
  closeSync,
  constants,
  existsSync,
  fstatSync,
  fsyncSync,
  lstatSync,
  mkdirSync,
  openSync,
  readFileSync,
  readdirSync,
  renameSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { basename, join } from "node:path";
import { createSqliteSnapshot } from "../../../coord/src/db/snapshot.ts";
import type { RegistryCoordinator } from "./registry.ts";
import { instanceLayoutFor } from "./layout.ts";

const RETAIN_DAILY = 14;
const OUTPUT_LIMIT = 1024 * 1024;

export interface BackupExecutor {
  snapshot(databasePath: string, targetPath: string): Promise<void>;
  encrypt(tarArgv: readonly string[], ageArgv: readonly string[], destination: string): Promise<void>;
  verify(destination: string, identityFile: string): Promise<readonly string[]>;
}

export interface EncryptedBackupOptions {
  rootDir: string;
  ageRecipient: string;
  ageIdentityFile: string;
  executor?: BackupExecutor;
  now?: () => Date;
}

async function defaultSnapshot(databasePath: string, targetPath: string): Promise<void> {
  const sqlite = new Database(databasePath, { readonly: true });
  try { createSqliteSnapshot(sqlite, targetPath); }
  finally { sqlite.close(true); }
}

async function capturedText(stream: ReadableStream<Uint8Array>): Promise<string> {
  const bytes = await new Response(stream).arrayBuffer();
  if (bytes.byteLength > OUTPUT_LIMIT) throw new Error("backup command output exceeded its bound");
  return new TextDecoder().decode(bytes);
}

interface CommandPipelineResult {
  producerExitCode: number;
  consumerExitCode: number;
  consumerStdout: string;
  producerStderr: string;
  consumerStderr: string;
}

async function runCommandPipeline(
  producerArgv: readonly string[],
  consumerArgv: readonly string[],
): Promise<CommandPipelineResult> {
  const producer = Bun.spawn([...producerArgv], {
    stdin: "ignore",
    stdout: "pipe",
    stderr: "pipe",
  });
  const consumer = Bun.spawn([...consumerArgv], {
    stdin: producer.stdout,
    stdout: "pipe",
    stderr: "pipe",
  });
  const [producerExitCode, consumerExitCode, consumerStdout, producerStderr, consumerStderr] =
    await Promise.all([
      producer.exited,
      consumer.exited,
      capturedText(consumer.stdout),
      capturedText(producer.stderr),
      capturedText(consumer.stderr),
    ]);
  return {
    producerExitCode,
    consumerExitCode,
    consumerStdout,
    producerStderr,
    consumerStderr,
  };
}

async function defaultEncrypt(
  tarArgv: readonly string[],
  ageArgv: readonly string[],
  _destination: string,
): Promise<void> {
  const result = await runCommandPipeline(tarArgv, ageArgv);
  if (result.producerExitCode !== 0 || result.consumerExitCode !== 0) {
    throw new Error("encrypted backup pipeline failed");
  }
}

async function defaultVerify(destination: string, identityFile: string): Promise<readonly string[]> {
  const result = await runCommandPipeline(
    ["age", "--decrypt", "--identity", identityFile, destination],
    ["tar", "-tf", "-"],
  );
  if (result.producerExitCode !== 0 || result.consumerExitCode !== 0) {
    throw new Error("encrypted backup verification failed");
  }
  return result.consumerStdout.split("\n").map((line) => line.trim()).filter(Boolean);
}

const DEFAULT_EXECUTOR: BackupExecutor = {
  snapshot: defaultSnapshot,
  encrypt: defaultEncrypt,
  verify: defaultVerify,
};

function timestampTag(now: Date): string {
  if (!Number.isFinite(now.getTime())) throw new Error("backup clock returned an invalid date");
  return now.toISOString().replace(/[:.]/g, "-").replace(/Z$/, "Z");
}

function copySecureInput(source: string, destination: string, maxBytes: number): void {
  const sourceFd = openSync(source, constants.O_RDONLY | constants.O_NOFOLLOW);
  try {
    const stat = fstatSync(sourceFd);
    if (!stat.isFile() || stat.size < 0 || stat.size > maxBytes || (stat.mode & 0o077) !== 0) {
      throw new Error(`backup input is invalid: ${basename(source)}`);
    }
    const bytes = readFileSync(sourceFd);
    writeFileSync(destination, bytes, { flag: "wx", mode: 0o600 });
    const destinationFd = openSync(destination, constants.O_RDONLY | constants.O_NOFOLLOW);
    try { fsyncSync(destinationFd); } finally { closeSync(destinationFd); }
  } finally {
    closeSync(sourceFd);
  }
}

async function snapshotBoundedDatabase(
  databasePath: string,
  snapshotPath: string,
  description: string,
  executor: BackupExecutor,
): Promise<void> {
  const databaseFd = openSync(databasePath, constants.O_RDONLY | constants.O_NOFOLLOW);
  try {
    const stat = fstatSync(databaseFd);
    if (!stat.isFile() || stat.size < 0 || stat.size > 1024 * 1024 * 1024) {
      throw new Error(`${description} database is not a bounded regular file`);
    }
    await executor.snapshot(databasePath, snapshotPath);
  } finally {
    closeSync(databaseFd);
  }
  chmodSync(snapshotPath, 0o600);
}

function makeRegistrySnapshotRestartable(snapshotPath: string, nowMs: number): void {
  const sqlite = new Database(snapshotPath);
  try {
    sqlite.exec("PRAGMA journal_mode=DELETE");
    sqlite.exec("BEGIN IMMEDIATE");
    try {
      sqlite.exec("DELETE FROM operation_leases; DELETE FROM global_leases");
      sqlite.query(`
        UPDATE provisioning_jobs
        SET state = 'pending',
            next_attempt_at_ms = MIN(next_attempt_at_ms, ?),
            locked_until_ms = NULL,
            lease_token = NULL,
            updated_at_ms = MAX(updated_at_ms, ?)
        WHERE state = 'running'
      `).run(nowMs, nowMs);
      const replayTable = sqlite.query<{ name: string }, [string]>(
        "SELECT name FROM sqlite_master WHERE type = 'table' AND name = ?",
      ).get("provisioner_ipc_replays");
      if (replayTable) {
        sqlite.exec("DELETE FROM provisioner_ipc_replays WHERE response_bytes IS NULL");
      }
      sqlite.exec("COMMIT");
    } catch (error) {
      try { sqlite.exec("ROLLBACK"); } catch { /* preserve the sanitization failure */ }
      throw error;
    }
    const integrity = sqlite.query<{ integrity_check: string }, []>("PRAGMA integrity_check").get();
    if (integrity?.integrity_check !== "ok") throw new Error("registry backup integrity check failed");
  } finally {
    sqlite.close();
  }
}

function removeStaleIntermediates(backupDir: string): void {
  for (const stale of readdirSync(backupDir).filter(
    (name) => /^\..+\.(?:snapshot\.db|tar\.age\.tmp|inputs)$/.test(name),
  )) {
    const stalePath = join(backupDir, stale);
    const stat = lstatSync(stalePath);
    if (!stat.isSymbolicLink() && (stat.isFile() || stat.isDirectory())) {
      rmSync(stalePath, { recursive: stat.isDirectory(), force: true });
    }
  }
}

function retainDailyBackups(backupDir: string): void {
  const backups = readdirSync(backupDir).filter((name) => name.endsWith(".tar.age")).sort();
  const byDay = new Map<string, string[]>();
  for (const name of backups) {
    const day = name.slice(0, 10);
    const names = byDay.get(day) ?? [];
    names.push(name);
    byDay.set(day, names);
  }
  const retainedDays = new Set([...byDay.keys()].sort().slice(-RETAIN_DAILY));
  for (const [day, names] of byDay) {
    const newest = names.at(-1);
    for (const name of names) {
      if (!retainedDays.has(day) || name !== newest) rmSync(join(backupDir, name), { force: true });
    }
  }
}

export async function createEncryptedBackup(
  coordinator: RegistryCoordinator,
  options: EncryptedBackupOptions,
): Promise<string> {
  const executor = options.executor ?? DEFAULT_EXECUTOR;
  const layout = instanceLayoutFor(coordinator);
  const databasePath = join(layout.dataDir, "coordinator_v2.db");
  const registryPath = join(options.rootDir, "control.db");
  if (!existsSync(registryPath)) throw new Error("SaaS registry database is missing");
  if (!existsSync(databasePath)) throw new Error("coordinator database is missing");

  const backupDir = join(options.rootDir, "backups", coordinator.id);
  mkdirSync(backupDir, { recursive: true, mode: 0o700 });
  chmodSync(backupDir, 0o700);
  removeStaleIntermediates(backupDir);

  const backupTime = (options.now ?? (() => new Date()))();
  const tag = timestampTag(backupTime);
  const destination = join(backupDir, `${tag}.tar.age`);
  const temporaryDestination = join(backupDir, `.${tag}.tar.age.tmp`);
  const stagingDir = join(backupDir, `.${tag}.inputs`);
  const snapshotPath = join(stagingDir, "coordinator.db");
  const registrySnapshotPath = join(stagingDir, "control.db");
  if (existsSync(destination)) throw new Error("backup destination already exists");

  try {
    mkdirSync(stagingDir, { mode: 0o700 });
    copySecureInput(layout.authorizedKeysPath, join(stagingDir, "authorized_keys.roost"), 1024 * 1024);
    copySecureInput(layout.manifestPath, join(stagingDir, "instance.json"), 64 * 1024);
    copySecureInput(layout.coordinatorKeyPath, join(stagingDir, "ssh_ed25519.key"), 64 * 1024);
    copySecureInput(layout.outboxKeyPath, join(stagingDir, "email-outbox-key"), 64 * 1024);

    await snapshotBoundedDatabase(databasePath, snapshotPath, "coordinator", executor);
    await snapshotBoundedDatabase(registryPath, registrySnapshotPath, "registry", executor);
    makeRegistrySnapshotRestartable(registrySnapshotPath, backupTime.getTime());

    const tarArgv = [
      "tar", "-cf", "-", "-C", stagingDir,
      "control.db", "coordinator.db", "authorized_keys.roost", "instance.json",
      "ssh_ed25519.key", "email-outbox-key",
    ];
    const ageArgv = ["age", "--recipient", options.ageRecipient, "--output", temporaryDestination];
    await executor.encrypt(tarArgv, ageArgv, temporaryDestination);
    chmodSync(temporaryDestination, 0o600);
    const entries = new Set(await executor.verify(temporaryDestination, options.ageIdentityFile));
    for (const required of [
      "control.db",
      "coordinator.db",
      "authorized_keys.roost",
      "instance.json",
      "ssh_ed25519.key",
      "email-outbox-key",
    ]) {
      if (!entries.has(required)) throw new Error(`verified backup is missing ${required}`);
    }
    if (entries.has(basename(layout.resendApiKeyPath))) {
      throw new Error("verified backup contains the shared Resend key");
    }

    const archiveFd = openSync(temporaryDestination, constants.O_RDONLY | constants.O_NOFOLLOW);
    try { fsyncSync(archiveFd); } finally { closeSync(archiveFd); }
    renameSync(temporaryDestination, destination);
    const directoryFd = openSync(backupDir, constants.O_RDONLY | constants.O_DIRECTORY);
    try { fsyncSync(directoryFd); } finally { closeSync(directoryFd); }
  } catch (error) {
    rmSync(destination, { force: true });
    rmSync(temporaryDestination, { force: true });
    throw error;
  } finally {
    rmSync(stagingDir, { recursive: true, force: true });
  }

  retainDailyBackups(backupDir);
  return destination;
}

export const _backupInternals = { runCommandPipeline };
