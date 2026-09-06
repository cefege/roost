// Captures filesystem proofs used by the agent-integration installer.
// Planning and commit both use these identities so case aliases, symlinks,
// directory swaps, and target ownership changes fail closed.

import type { Stats } from "node:fs";
import { lstat, readFile, realpath } from "node:fs/promises";
import { basename, dirname, join, resolve } from "node:path";
import type { SupportedHostPlatform } from "@roost/shared/platform";

export interface IntegrationFileSnapshot {
  content: string;
  device: number;
  inode: number;
}

export interface IntegrationDirectorySnapshot {
  canonicalPath: string;
  entryIsSymlink: boolean;
  entryDevice: number;
  entryInode: number;
  directoryDevice: number;
  directoryInode: number;
}

export interface IntegrationDirectoryPlan {
  path: string;
  canonicalPath: string;
  initialSnapshot: IntegrationDirectorySnapshot | null;
}

export interface IntegrationTargetPlan {
  target: string;
  ownershipMarker: string;
  existing: IntegrationFileSnapshot | null;
  remove?: boolean;
}

export function integrationPathComparisonKey(
  path: string,
  platform: SupportedHostPlatform,
): string {
  const normalized = path.normalize("NFC");
  return platform === "darwin" || platform === "win32"
    ? normalized.toLocaleLowerCase("en-US")
    : normalized;
}

export function hasIntegrationOwnership(content: string, marker: string): boolean {
  return content.split(/\r?\n/, 8).some((line) =>
    line.startsWith("//") &&
    line.slice(2).trim().split(/\s+/).includes(marker)
  );
}

export async function preflightIntegrationDirectory(
  path: string,
): Promise<IntegrationDirectoryPlan> {
  try {
    const snapshot = await inspectIntegrationDirectory(path);
    return { path, canonicalPath: snapshot.canonicalPath, initialSnapshot: snapshot };
  } catch (error) {
    if (integrationErrorCode(error) !== "ENOENT") throw error;
    return {
      path,
      canonicalPath: await canonicalizePlannedPath(path),
      initialSnapshot: null,
    };
  }
}

export async function inspectIntegrationTarget(
  path: string,
  description: string,
): Promise<IntegrationFileSnapshot | null> {
  let before: Stats;
  try {
    before = await lstat(path);
  } catch (error) {
    if (integrationErrorCode(error) === "ENOENT") return null;
    throw error;
  }
  if (before.isSymbolicLink()) {
    throw new Error(`refusing symlink ${description}: ${path}`);
  }
  if (!before.isFile()) {
    throw new Error(`refusing unsafe ${description}: ${path}`);
  }
  const content = await readFile(path, "utf8");
  const after = await lstat(path);
  if (
    !after.isFile() ||
    after.isSymbolicLink() ||
    !sameIntegrationIdentity(before, { device: after.dev, inode: after.ino })
  ) {
    throw new Error(`${description} changed while being inspected: ${path}`);
  }
  return { content, device: after.dev, inode: after.ino };
}

export async function assertIntegrationTargetUnchanged(
  plan: IntegrationTargetPlan,
): Promise<void> {
  let current: IntegrationFileSnapshot | null;
  try {
    current = await inspectIntegrationTarget(plan.target, "agent integration target");
  } catch {
    throw new Error(`agent integration target changed before commit: ${plan.target}`);
  }
  if (!sameIntegrationFileSnapshot(current, plan.existing)) {
    throw new Error(`agent integration target changed before commit: ${plan.target}`);
  }
  if (current && !hasIntegrationOwnership(current.content, plan.ownershipMarker)) {
    if (plan.remove === false) return;
    throw new Error(`agent integration target ownership changed before commit: ${plan.target}`);
  }
}

export async function inspectIntegrationDirectory(
  path: string,
): Promise<IntegrationDirectorySnapshot> {
  const entry = await lstat(path);
  let canonicalPath: string;
  try {
    canonicalPath = await realpath(path);
  } catch {
    throw new Error(`refusing dangling agent integration loader: ${path}`);
  }
  const directory = await lstat(canonicalPath);
  if (!directory.isDirectory()) {
    throw new Error(`refusing non-directory agent integration loader: ${path}`);
  }
  return {
    canonicalPath,
    entryIsSymlink: entry.isSymbolicLink(),
    entryDevice: entry.dev,
    entryInode: entry.ino,
    directoryDevice: directory.dev,
    directoryInode: directory.ino,
  };
}

export async function assertIntegrationDirectorySnapshot(
  plan: IntegrationDirectoryPlan,
  expected: IntegrationDirectorySnapshot,
  platform: SupportedHostPlatform,
): Promise<void> {
  let current: IntegrationDirectorySnapshot;
  try {
    current = await inspectIntegrationDirectory(plan.path);
  } catch {
    throw new Error(`agent integration loader changed during installation: ${plan.path}`);
  }
  if (
    current.entryIsSymlink !== expected.entryIsSymlink ||
    current.entryDevice !== expected.entryDevice ||
    current.entryInode !== expected.entryInode ||
    current.directoryDevice !== expected.directoryDevice ||
    current.directoryInode !== expected.directoryInode ||
    integrationPathComparisonKey(current.canonicalPath, platform) !==
      integrationPathComparisonKey(expected.canonicalPath, platform)
  ) {
    throw new Error(`agent integration loader changed during installation: ${plan.path}`);
  }
}

export async function integrationLstatIfPresent(path: string): Promise<Stats | null> {
  try {
    return await lstat(path);
  } catch (error) {
    if (integrationErrorCode(error) === "ENOENT") return null;
    throw error;
  }
}

export function sameIntegrationFileSnapshot(
  left: IntegrationFileSnapshot | null,
  right: IntegrationFileSnapshot | null,
): boolean {
  if (left === null || right === null) return left === right;
  return left.content === right.content &&
    left.device === right.device &&
    left.inode === right.inode;
}

export function sameIntegrationIdentity(
  left: Pick<Stats, "dev" | "ino">,
  right: { device: number; inode: number },
): boolean {
  return left.dev === right.device && left.ino === right.inode;
}

export function integrationErrorCode(error: unknown): unknown {
  return error && typeof error === "object" && "code" in error
    ? error.code
    : undefined;
}

async function canonicalizePlannedPath(path: string): Promise<string> {
  const absolute = resolve(path);
  try {
    return await realpath(absolute);
  } catch (error) {
    if (integrationErrorCode(error) !== "ENOENT") throw error;
    const parent = dirname(absolute);
    if (parent === absolute) throw error;
    return join(await canonicalizePlannedPath(parent), basename(absolute));
  }
}
