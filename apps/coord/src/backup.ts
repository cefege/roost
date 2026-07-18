// Nightly DB backup. Copies coord DB to <backups_dir>/coord_v2.<timestamp>.db.gz.
// Keeps the 14 most recent backups; older files are deleted.
// backups_dir = dirname(dbPath) + "/backups".
// Callers: main.ts (startup + setInterval 24h).

import { existsSync, mkdirSync, readdirSync, unlinkSync, statSync } from "node:fs";
import { join, dirname } from "node:path";
import { log } from "@roost/shared/log";

const MAX_BACKUPS = 14;
const BACKUP_INTERVAL_MS = 24 * 60 * 60 * 1000;

function backupsDir(dbPath: string): string {
  return join(dirname(dbPath), "backups");
}

function timestampTag(): string {
  return new Date().toISOString().replace(/[:.]/g, "-").slice(0, 19);
}

function listBackups(dir: string): string[] {
  if (!existsSync(dir)) return [];
  return readdirSync(dir)
    .filter((f) => f.startsWith("coord_v2.") && f.endsWith(".db.gz"))
    .map((f) => join(dir, f))
    .sort(); // ISO timestamp prefix sorts chronologically
}

async function runBackup(dbPath: string): Promise<void> {
  const dir = backupsDir(dbPath);
  mkdirSync(dir, { recursive: true });

  const tag = timestampTag();
  const destPath = join(dir, `coord_v2.${tag}.db.gz`);

  try {
    // Bun-native gzip (NOT node:zlib — heap-corruption/segfault class, see
    // main.ts). The coord DB is small enough to gzip in one buffer and backups
    // aren't latency-sensitive, so a streaming transform isn't worth a node:zlib
    // dependency.
    const raw = new Uint8Array(await Bun.file(dbPath).arrayBuffer());
    await Bun.write(destPath, Bun.gzipSync(raw));
    log.info("backup", "backup_written", { path: destPath });
  } catch (err) {
    log.error("backup", "backup_failed", { error: (err as Error).message, destPath });
    // Remove partial file on failure.
    try { unlinkSync(destPath); } catch { /* ignore */ }
    return;
  }

  // Prune: keep newest MAX_BACKUPS, delete the rest.
  const all = listBackups(dir);
  const excess = all.slice(0, Math.max(0, all.length - MAX_BACKUPS));
  for (const old of excess) {
    try {
      unlinkSync(old);
      log.info("backup", "backup_pruned", { path: old });
    } catch (err) {
      log.warn("backup", "backup_prune_failed", { path: old, error: (err as Error).message });
    }
  }
}

// Returns true if the most recent backup is older than 24h (or none exist).
function backupStale(dbPath: string): boolean {
  const dir = backupsDir(dbPath);
  const all = listBackups(dir);
  if (all.length === 0) return true;
  const latest = all.at(-1)!;
  try {
    const { mtimeMs } = statSync(latest);
    return Date.now() - mtimeMs > BACKUP_INTERVAL_MS;
  } catch {
    return true;
  }
}

// Schedules a 24h recurring backup. Runs one immediately at startup if stale.
export function scheduleBackups(dbPath: string): void {
  if (!existsSync(dbPath)) {
    log.warn("backup", "backup_skip_no_db", { dbPath });
    return;
  }

  if (backupStale(dbPath)) {
    void runBackup(dbPath);
  }

  setInterval(() => {
    void runBackup(dbPath);
  }, BACKUP_INTERVAL_MS).unref();
}
