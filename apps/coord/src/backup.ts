// Nightly DB backup. Creates a verified SQLite snapshot, then compresses it to
// <backups_dir>/coord_v2.<timestamp>.db.gz. Keeps the 14 most recent backups.

import {
  chmodSync,
  existsSync,
  mkdirSync,
  readdirSync,
  renameSync,
  rmSync,
  statSync,
} from "node:fs";
import { dirname, join } from "node:path";
import { log } from "@roost/shared/log";
import type { Database } from "bun:sqlite";
import { createSqliteSnapshot } from "./db/snapshot.ts";
import { DAY_MS } from "./audit-retention.ts";

const MAX_BACKUPS = 14;

export interface InVolumeBackupOptions {
  managedContainer?: boolean;
}

/** Managed instances are backed up by the host lifecycle, never into /data. */
export function makePreMigrationBackupHook(
  sqlite: Database,
  dbPath: string,
  options: InVolumeBackupOptions = {},
): (() => Promise<void>) | undefined {
  if (options.managedContainer) return undefined;
  return async () => {
    await runBackup(sqlite, dbPath, "pre-migration");
  };
}


function listBackups(dir: string): string[] {
  if (!existsSync(dir)) return [];
  return readdirSync(dir)
    .filter((file) => file.startsWith("coord_v2.") && file.endsWith(".db.gz"))
    .map((file) => join(dir, file))
    .sort();
}

export async function runBackup(
  sqlite: Database,
  dbPath: string,
  reason: "scheduled" | "pre-migration",
): Promise<string> {
  const dir = join(dirname(dbPath), "backups");

  const tag = new Date().toISOString().replace(/[:.]/g, "-").replace(/Z$/, "");
  const destPath = join(dir, `coord_v2.${tag}.db.gz`);
  const snapshotPath = join(dir, `.coord_v2.${tag}.snapshot.db`);
  const archivePath = join(dir, `.coord_v2.${tag}.db.gz.tmp`);

  try {
    mkdirSync(dir, { recursive: true, mode: 0o700 });
    chmodSync(dir, 0o700);
    createSqliteSnapshot(sqlite, snapshotPath);
    const raw = new Uint8Array(await Bun.file(snapshotPath).arrayBuffer());
    await Bun.write(archivePath, Bun.gzipSync(raw));
    chmodSync(archivePath, 0o600);
    renameSync(archivePath, destPath);
    chmodSync(destPath, 0o600);
    log.info("backup", "backup_written", { path: destPath, reason });
  } catch (error) {
    try { rmSync(archivePath, { force: true }); } catch { /* parent may be unusable */ }
    try { rmSync(destPath, { force: true }); } catch { /* parent may be unusable */ }
    log.error("backup", "backup_failed", {
      error: (error as Error).message,
      destPath,
      reason,
    });
    throw error;
  } finally {
    try { rmSync(snapshotPath, { force: true }); } catch { /* parent may be unusable */ }
  }

  const all = listBackups(dir);
  const excess = all.slice(0, Math.max(0, all.length - MAX_BACKUPS));
  for (const old of excess) {
    try {
      rmSync(old, { force: true });
      log.info("backup", "backup_pruned", { path: old });
    } catch (error) {
      log.warn("backup", "backup_prune_failed", {
        path: old,
        error: (error as Error).message,
      });
    }
  }
  return destPath;
}

function backupStale(dbPath: string): boolean {
  const dir = join(dirname(dbPath), "backups");
  const all = listBackups(dir);
  if (all.length === 0) return true;
  try {
    return Date.now() - statSync(all.at(-1)!).mtimeMs > DAY_MS;
  } catch {
    return true;
  }
}

function runScheduledBackup(sqlite: Database, dbPath: string): void {
  void runBackup(sqlite, dbPath, "scheduled").catch(() => {
    // runBackup logged the actionable error; scheduled work has no caller.
  });
}

export function scheduleBackups(
  sqlite: Database,
  dbPath: string,
  options: InVolumeBackupOptions = {},
): void {
  if (options.managedContainer) return;
  if (!existsSync(dbPath)) {
    log.warn("backup", "backup_skip_no_db", { dbPath });
    return;
  }
  if (backupStale(dbPath)) runScheduledBackup(sqlite, dbPath);
  setInterval(() => runScheduledBackup(sqlite, dbPath), DAY_MS).unref();
}
