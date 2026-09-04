// Coordinator target contracts define relocation state, injected runtimes, and validation.
// Snapshot and lifecycle modules share these types while CoordTarget remains the public facade.
// Key, database, disk, and advertised-target checks stay centralized across POSIX and Windows.

import * as fs from "node:fs";
import { dirname } from "node:path";
import { fingerprintOf } from "@roost/shared/fingerprint";
import { resolveTailnetDnsName } from "@roost/shared/tailnet";
import { Database } from "bun:sqlite";
import { coordServiceLabel } from "@roost/shared/paths";
import { ROOST_BUILD_SHA } from "@roost/shared/build-identity";
import type { WindowsCoordinatorPromotionRelocationOperation } from "@roost/shared/windows-relocation";
import {
  createDefaultWindowsCoordRuntime,
  type WindowsCoordRuntime,
} from "./coord-relocation-windows-runtime.ts";
import type { WindowsCoordinatorTargetRelocation } from "./coord-relocation-windows.ts";

export const coordLabel = (): string => coordServiceLabel();

export interface CoordTargetPaths {
  dataDir: string;
  dbPath: string;
  keyPath: string;
  authorizedKeysPath: string;
  handoffPath: string;
  /** launchd plist, systemd unit, or Windows service-definition asset. */
  servicePath: string;
}

export interface InflightSnapshot {
  handoffId: string;
  file: string;
  fd: number;
  expectedSize: number;
  expectedSha256: string;
  expectedWorkerFps: string[];
  secretSha256: string;
  nextSeq: number;
  received: number;
  hasher: Bun.CryptoHasher;
}

export interface PreparedTarget {
  handoffId: string;
  sourceUrl: string;
  targetUrl: string;
  expectedCoordKid: string;
  expectedGitSha: string;
  windowsRelocation?: WindowsCoordinatorPromotionRelocationOperation;
}

export interface RollbackPresent {
  db: boolean;
  key: boolean;
  authorizedKeys: boolean;
  handoff: boolean;
  service: boolean;
}

export interface CoordTargetRuntime {
  platform: string;
  gitSha: string;
  tailnetDnsName(): string;
  isCoordServiceActive(label: string): Promise<boolean>;
  restoreCoordService(label: string, servicePath: string): Promise<void>;
  coordHealthy?(targetUrl: string): Promise<boolean>;
  windows?: WindowsCoordRuntime;
}

export interface CoordTargetContext {
  readonly paths: CoordTargetPaths;
  readonly runtime: CoordTargetRuntime;
  inflight: InflightSnapshot | null;
  prepared: PreparedTarget | null;
  readonly windows: WindowsCoordinatorTargetRelocation | null;
  abort(handoffId: string): Promise<void>;
}

export interface CoordTargetPrepareRequest {
  handoff_id: string;
  source_url: string;
  target_url: string;
  expected_coord_kid: string;
  action: "CHECK" | "PREPARE";
  estimated_db_size: bigint;
  expected_git_sha: string;
}

export interface CoordTargetStartSnapshotRequest {
  request_id: string;
  handoff_id: string;
  total_size: bigint;
  sha256: string;
  coord_key_pem: Uint8Array;
  authorized_keys: Uint8Array;
  secret_sha256: string;
  expected_worker_fps: string[];
}

export interface CoordTargetSnapshotChunk {
  handoff_id: string;
  seq: number;
  data: Uint8Array;
  last: boolean;
}

function systemdEnv(): Record<string, string | undefined> {
  return {
    ...process.env,
    XDG_RUNTIME_DIR: process.env.XDG_RUNTIME_DIR ?? `/run/user/${process.getuid?.() ?? 0}`,
  };
}

export async function defaultCoordHealthy(targetUrl: string): Promise<boolean> {
  const response = await fetch(
    `${targetUrl.replace(/\/$/, "")}/roost.v1.CoordinatorService/MiscHealth`,
    {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: "{}",
    },
  ).catch(() => null);
  return response?.ok === true;
}

export const defaultRuntime: CoordTargetRuntime = {
  platform: process.platform,
  gitSha: ROOST_BUILD_SHA !== "dev"
    ? ROOST_BUILD_SHA
    : process.env.GIT_SHA ?? process.env.ROOST_GIT_SHA ?? "dev",
  tailnetDnsName: resolveTailnetDnsName,
  async isCoordServiceActive(label) {
    switch (process.platform) {
      case "darwin": {
        const child = Bun.spawn(
          ["launchctl", "print", `gui/${process.getuid?.() ?? 0}/${label}`],
          { stdout: "ignore", stderr: "ignore" },
        );
        return await child.exited === 0;
      }
      case "linux": {
        const child = Bun.spawn(["systemctl", "--user", "is-active", `${label}.service`], {
          stdout: "ignore",
          stderr: "ignore",
          env: systemdEnv(),
        });
        return await child.exited === 0;
      }
      case "win32":
        return (await createDefaultWindowsCoordRuntime().queryService("coordinator")).state === "running";
      default:
        throw new Error(`unsupported coordinator target platform: ${process.platform}`);
    }
  },
  async restoreCoordService(label, servicePath) {
    switch (process.platform) {
      case "darwin": {
        const uid = process.getuid?.() ?? 0;
        const bootout = Bun.spawn(
          ["launchctl", "bootout", `gui/${uid}/${label}`],
          { stdout: "ignore", stderr: "ignore" },
        );
        await bootout.exited;
        const bootstrap = Bun.spawn(
          ["launchctl", "bootstrap", `gui/${uid}`, servicePath],
          { stdout: "ignore", stderr: "ignore" },
        );
        if (await bootstrap.exited !== 0) {
          throw new Error("failed to restore prior coordinator LaunchAgent");
        }
        return;
      }
      case "linux": {
        const reload = Bun.spawn(["systemctl", "--user", "daemon-reload"], {
          stdout: "ignore",
          stderr: "ignore",
          env: systemdEnv(),
        });
        await reload.exited;
        const restart = Bun.spawn(["systemctl", "--user", "restart", `${label}.service`], {
          stdout: "ignore",
          stderr: "ignore",
          env: systemdEnv(),
        });
        if (await restart.exited !== 0) {
          throw new Error("failed to restore prior coordinator systemd unit");
        }
        return;
      }
      case "win32":
        throw new Error("Windows coordinator restoration must use the relocation service transaction");
      default:
        throw new Error(`unsupported coordinator target platform: ${process.platform}`);
    }
  },
  windows: process.platform === "win32" ? createDefaultWindowsCoordRuntime() : undefined,
};

export async function coordinatorKeyFingerprint(pem: Uint8Array): Promise<string> {
  const encoded = Buffer.from(pem)
    .toString("utf8")
    .replace(/-----[^-]+-----/g, "")
    .replace(/\s+/g, "");
  const raw = Buffer.from(encoded, "base64");
  const magic = Buffer.from("openssh-key-v1\0", "ascii");
  if (!raw.subarray(0, magic.length).equals(magic)) {
    throw new Error("coordinator key is not an OpenSSH key");
  }
  let position = magic.length;
  const readU32 = (): number => {
    const value = raw.readUInt32BE(position);
    position += 4;
    return value;
  };
  const readString = (): Buffer => {
    const length = readU32();
    const value = raw.subarray(position, position + length);
    position += length;
    return value;
  };
  readString();
  readString();
  readString();
  readU32();
  readString();
  const privateBlock = readString();
  let privatePosition = 8;
  const readPrivateString = (): Buffer => {
    const length = privateBlock.readUInt32BE(privatePosition);
    privatePosition += 4;
    const value = privateBlock.subarray(privatePosition, privatePosition + length);
    privatePosition += length;
    return value;
  };
  if (readPrivateString().toString("utf8") !== "ssh-ed25519") {
    throw new Error("coordinator key is not ed25519");
  }
  const publicKey = readPrivateString();
  if (publicKey.length !== 32) {
    throw new Error("coordinator key has an invalid public key");
  }
  return fingerprintOf(publicKey);
}

export function existingDirectory(path: string): string {
  let candidate = path;
  while (!fs.existsSync(candidate)) {
    const parent = dirname(candidate);
    if (parent === candidate) throw new Error(`no existing directory for ${path}`);
    candidate = parent;
  }
  return candidate;
}

export function assertWritableDirectory(path: string, mustExist = false): void {
  if (mustExist && !fs.existsSync(path)) throw new Error(`${path} does not exist`);
  const directory = existingDirectory(path);
  if (!fs.statSync(directory).isDirectory()) throw new Error(`${directory} is not a directory`);
  fs.accessSync(directory, fs.constants.W_OK);
}

export function dashboardIdFromVerifiedSnapshot(
  snapshotPath: string,
  expectedWorkerFps: string[],
): string {
  const expected = new Set(expectedWorkerFps);
  if (expected.size === 0 || expected.size !== expectedWorkerFps.length) {
    throw new Error("coordinator snapshot has an invalid expected worker set");
  }
  const snapshot = new Database(snapshotPath, { readonly: true });
  try {
    const integrity = snapshot.query("PRAGMA integrity_check").get() as {
      integrity_check?: string;
    } | null;
    if (integrity?.integrity_check !== "ok") {
      throw new Error("coordinator snapshot integrity check failed");
    }
    const placeholders = expectedWorkerFps.map(() => "?").join(", ");
    const rows = snapshot
      .query(`SELECT fp, dashboard_id FROM workers WHERE fp IN (${placeholders})`)
      .all(...expectedWorkerFps) as Array<{ fp: string; dashboard_id: string | null }>;
    if (rows.length !== expected.size) {
      throw new Error("coordinator snapshot is missing expected workers");
    }
    const dashboardIds = new Set<string>();
    for (const worker of rows) {
      if (
        !expected.has(worker.fp)
        || typeof worker.dashboard_id !== "string"
        || worker.dashboard_id.length === 0
      ) {
        throw new Error("coordinator snapshot has an unscoped expected worker");
      }
      dashboardIds.add(worker.dashboard_id);
    }
    if (dashboardIds.size !== 1) {
      throw new Error("coordinator snapshot spans multiple dashboards");
    }
    return dashboardIds.values().next().value!;
  } finally {
    snapshot.close();
  }
}

export function targetHandoffPayload(
  prepared: PreparedTarget,
  inflight: InflightSnapshot,
  dashboardId: string,
): string {
  const now = Date.now();
  return JSON.stringify({
    version: 1,
    handoff_id: inflight.handoffId,
    role: "TARGET",
    dashboard_id: dashboardId,
    phase: "WAITING_FOR_WORKERS",
    source_url: prepared.sourceUrl,
    target_url: prepared.targetUrl,
    target_worker_fp: "target-pending",
    expected_worker_fps: inflight.expectedWorkerFps,
    commit_acked_worker_fps: [],
    expected_coord_kid: prepared.expectedCoordKid,
    expected_git_sha: prepared.expectedGitSha,
    secret_sha256: inflight.secretSha256,
    started_at_ms: now,
    updated_at_ms: now,
  });
}
