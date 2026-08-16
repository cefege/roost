import { randomUUID } from "node:crypto";
import { mkdir, open, readFile, rename, rm } from "node:fs/promises";
import { join, win32 } from "node:path";
import { applyPrivateDacl, durableRemove, type DurabilityOptions } from "./durability.ts";
import { roostServiceDir, type PathEnv } from "./paths.ts";
import { supportedHostPlatform, type SupportedHostPlatform } from "./platform.ts";
import { windowsProcessSnapshot } from "./windows-helper.ts";

export type MachineTransactionKind = "update" | "relocation" | "keeper-refresh";

export interface MachineTransactionRecord {
  schemaVersion: 1;
  kind: MachineTransactionKind;
  journalPath: string;
  ownerPid: number;
  processEpoch: string;
  acquiredAt: string;
}

export interface AcquireMachineTransactionOptions extends DurabilityOptions {
  platform?: SupportedHostPlatform;
  lockPath?: string;
  processEpoch?: string;
  env?: PathEnv;
}

export interface MachineTransactionLock extends MachineTransactionRecord {
  lockPath: string;
  release(): Promise<void>;
}

export class MachineTransactionBusyError extends Error {
  readonly active: MachineTransactionRecord | null;
  constructor(lockPath: string, active: MachineTransactionRecord | null) {
    super(`machine transaction already active at ${lockPath}${active ? ` (${active.kind})` : ""}`);
    this.name = "MachineTransactionBusyError";
    this.active = active;
  }
}

function parseRecord(text: string): MachineTransactionRecord | null {
  try {
    const value = JSON.parse(text) as Partial<MachineTransactionRecord>;
    if (value.schemaVersion !== 1
      || (value.kind !== "update" && value.kind !== "relocation" && value.kind !== "keeper-refresh")
      || typeof value.journalPath !== "string"
      || !Number.isInteger(value.ownerPid) || value.ownerPid! <= 0
      || typeof value.processEpoch !== "string"
      || typeof value.acquiredAt !== "string") return null;
    return value as MachineTransactionRecord;
  } catch {
    return null;
  }
}

async function ownerIsAlive(record: MachineTransactionRecord, options: AcquireMachineTransactionOptions): Promise<boolean> {
  if (record.ownerPid === process.pid && record.processEpoch === options.processEpoch) return true;
  const platform = options.platform ?? supportedHostPlatform();
  if (platform === "win32") {
    const records = await windowsProcessSnapshot(options.helper);
    return records.some((candidate) => candidate.pid === record.ownerPid);
  }
  try {
    process.kill(record.ownerPid, 0);
    return true;
  } catch (error) {
    return (error as NodeJS.ErrnoException).code === "EPERM";
  }
}

async function readExistingRecord(lockPath: string): Promise<MachineTransactionRecord | null> {
  try {
    return parseRecord(await readFile(lockPath, "utf8"));
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === "ENOENT") return null;
    throw error;
  }
}

export async function acquireMachineTransaction(
  kind: MachineTransactionKind,
  journalPath: string,
  options: AcquireMachineTransactionOptions = {},
): Promise<MachineTransactionLock> {
  const platform = options.platform ?? supportedHostPlatform();
  const env = options.env ?? process.env;
  const base = roostServiceDir(env, platform);
  const lockPath = options.lockPath ?? (platform === "win32"
    ? win32.join(base, "machine-transaction.lock")
    : join(base, "machine-transaction.lock"));
  await mkdir(base, { recursive: true, mode: 0o700 });
  const processEpoch = options.processEpoch ?? randomUUID();
  const record: MachineTransactionRecord = {
    schemaVersion: 1,
    kind,
    journalPath,
    ownerPid: process.pid,
    processEpoch,
    acquiredAt: new Date().toISOString(),
  };
  for (let attempt = 0; attempt < 2; attempt++) {
    try {
      const handle = await open(lockPath, "wx", 0o600);
      try {
        await handle.writeFile(`${JSON.stringify(record)}\n`, "utf8");
        await handle.sync();
      } finally {
        await handle.close();
      }
      await applyPrivateDacl(lockPath, { ...options, platform });
      let released = false;
      return {
        ...record,
        lockPath,
        async release() {
          if (released) return;
          const current = await readExistingRecord(lockPath);
          if (!current || current.processEpoch !== processEpoch) {
            throw new Error("machine transaction ownership changed before release");
          }
          await durableRemove(lockPath, { ...options, platform });
          released = true;
        },
      };
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code !== "EEXIST") throw error;
      const active = await readExistingRecord(lockPath);
      if (!active || await ownerIsAlive(active, { ...options, platform, processEpoch })) {
        throw new MachineTransactionBusyError(lockPath, active);
      }
      const stalePath = `${lockPath}.stale-${randomUUID()}`;
      try {
        await rename(lockPath, stalePath);
        await rm(stalePath, { force: true });
      } catch (reclaimError) {
        if ((reclaimError as NodeJS.ErrnoException).code !== "ENOENT") {
          throw new MachineTransactionBusyError(lockPath, active);
        }
      }
    }
  }
  throw new MachineTransactionBusyError(lockPath, await readExistingRecord(lockPath));
}
