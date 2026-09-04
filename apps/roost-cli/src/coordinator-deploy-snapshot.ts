// Creates the coordinator rollout's compressed live SQLite snapshot and restores it.
// The deploy orchestrator snapshots before target activation. Rollback first stops
// the target, then verifies the exact archive bytes and SQLite integrity before a
// same-directory atomic replacement of the prior database.
import { createHash, randomUUID } from "node:crypto";
import {
  chmodSync,
  closeSync,
  constants,
  existsSync,
  fstatSync,
  lstatSync,
  openSync,
  realpathSync,
  rmSync,
} from "node:fs";
import { createReadStream, createWriteStream } from "node:fs";
import { dirname, resolve } from "node:path";
import { Transform } from "node:stream";
import { pipeline } from "node:stream/promises";
import { createGunzip, createGzip } from "node:zlib";
import { durableRemove, durableReplace } from "@roost/shared/durability";
import { Database } from "bun:sqlite";
import { createSqliteSnapshot } from "../../coord/src/db/snapshot.ts";

export interface CoordinatorRollbackSnapshot {
  sha256: string;
}

function assertCanonicalDirectory(path: string, description: string): void {
  if (resolve(path) !== path
    || !existsSync(path)
    || !lstatSync(path).isDirectory()
    || lstatSync(path).isSymbolicLink()
    || realpathSync(path) !== path) {
    throw new Error(`${description} must be a canonical real directory`);
  }
}

function assertCanonicalRegularFile(path: string, description: string): void {
  if (resolve(path) !== path || !existsSync(path)) {
    throw new Error(`${description} must be a canonical regular file`);
  }
  const metadata = lstatSync(path);
  if (!metadata.isFile() || metadata.isSymbolicLink() || realpathSync(path) !== path) {
    throw new Error(`${description} must be a canonical regular file`);
  }
}

function verifySqliteIntegrity(path: string, description: string): void {
  const sqlite = new Database(path, { readonly: true, strict: true });
  try {
    const rows = sqlite.query<{ integrity_check: string }, []>("PRAGMA integrity_check").all();
    if (rows.length !== 1 || rows[0]?.integrity_check !== "ok") {
      throw new Error(`${description} integrity_check failed: ${rows[0]?.integrity_check ?? "no result"}`);
    }
  } finally {
    sqlite.close(true);
  }
}

async function sha256File(path: string): Promise<string> {
  const digest = createHash("sha256");
  for await (const chunk of createReadStream(path)) digest.update(chunk as Buffer);
  return digest.digest("hex");
}

/** Snapshot a live database into a durable gzip archive without stopping its writer. */
export async function createCoordinatorRollbackSnapshot(
  databasePath: string,
  snapshotPath: string,
): Promise<CoordinatorRollbackSnapshot> {
  assertCanonicalRegularFile(databasePath, "coordinator database");
  if (resolve(snapshotPath) !== snapshotPath) {
    throw new Error("coordinator database snapshot path must be canonical and absolute");
  }
  assertCanonicalDirectory(dirname(snapshotPath), "coordinator snapshot directory");
  if (existsSync(snapshotPath)) throw new Error("coordinator database snapshot already exists");

  const nonce = `${process.pid}-${randomUUID()}`;
  const rawSnapshotPath = `${snapshotPath}.raw-${nonce}`;
  const compressedPath = `${snapshotPath}.tmp-${nonce}`;
  try {
    const sqlite = new Database(databasePath, { readonly: true, strict: true });
    try {
      createSqliteSnapshot(sqlite, rawSnapshotPath);
    } finally {
      sqlite.close(true);
    }
    await pipeline(
      createReadStream(rawSnapshotPath),
      createGzip(),
      createWriteStream(compressedPath, { flags: "wx", mode: 0o600 }),
    );
    chmodSync(compressedPath, 0o600);
    const sha256 = await sha256File(compressedPath);
    const verifiedDatabasePath = await materializeVerifiedSnapshot(
      databasePath,
      compressedPath,
      sha256,
    );
    rmSync(verifiedDatabasePath, { force: true });
    await durableReplace(compressedPath, snapshotPath, { mode: 0o600 });
    return { sha256 };
  } catch (error) {
    rmSync(snapshotPath, { force: true });
    throw error;
  } finally {
    rmSync(rawSnapshotPath, { force: true });
    rmSync(compressedPath, { force: true });
  }
}

async function materializeVerifiedSnapshot(
  databasePath: string,
  snapshotPath: string,
  expectedSha256: string,
): Promise<string> {
  assertCanonicalRegularFile(snapshotPath, "coordinator database snapshot");
  assertCanonicalDirectory(dirname(databasePath), "coordinator database directory");
  const temporaryDatabasePath = `${databasePath}.rollback-${process.pid}-${randomUUID()}`;
  const snapshotMetadata = lstatSync(snapshotPath);
  const descriptor = openSync(
    snapshotPath,
    constants.O_RDONLY | (constants.O_NOFOLLOW ?? 0),
  );
  try {
    const openedMetadata = fstatSync(descriptor);
    if (openedMetadata.dev !== snapshotMetadata.dev || openedMetadata.ino !== snapshotMetadata.ino) {
      throw new Error("coordinator database snapshot changed while it was opened");
    }
    const digest = createHash("sha256");
    const hashCompressedBytes = new Transform({
      transform(chunk, _encoding, callback) {
        digest.update(chunk);
        callback(null, chunk);
      },
    });
    await pipeline(
      createReadStream(snapshotPath, { fd: descriptor, autoClose: false }),
      hashCompressedBytes,
      createGunzip(),
      createWriteStream(temporaryDatabasePath, { flags: "wx", mode: 0o600 }),
    );
    const actualSha256 = digest.digest("hex");
    if (actualSha256 !== expectedSha256) {
      throw new Error(
        `coordinator database snapshot digest mismatch: expected ${expectedSha256}, got ${actualSha256}`,
      );
    }
    verifySqliteIntegrity(temporaryDatabasePath, "coordinator database snapshot");
    chmodSync(temporaryDatabasePath, 0o600);
    return temporaryDatabasePath;
  } catch (error) {
    rmSync(temporaryDatabasePath, { force: true });
    throw error;
  } finally {
    closeSync(descriptor);
  }
}

/** Stop the target, verify/decompress, then atomically install the prior DB. */
export async function restoreCoordinatorDatabaseFromSnapshot(
  databasePath: string,
  snapshotPath: string,
  expectedSha256: string,
  stopTarget: () => Promise<void>,
): Promise<void> {
  assertCanonicalRegularFile(databasePath, "coordinator database");
  await stopTarget();
  const restoredPath = await materializeVerifiedSnapshot(
    databasePath,
    snapshotPath,
    expectedSha256,
  );
  let committed = false;
  try {

    await durableRemove(`${databasePath}-wal`);
    await durableRemove(`${databasePath}-shm`);
    await durableReplace(restoredPath, databasePath, { mode: 0o600 });
    committed = true;
    chmodSync(databasePath, 0o600);
  } finally {
    if (!committed) rmSync(restoredPath, { force: true });
  }
}
