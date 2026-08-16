import { randomBytes } from "node:crypto";
import { chmod, mkdir, open, rename, rm } from "node:fs/promises";
import { dirname, win32 } from "node:path";
import { assertNeverPlatform, supportedHostPlatform, type SupportedHostPlatform } from "./platform.ts";
import { runWindowsHelper, type RunWindowsHelperOptions } from "./windows-helper.ts";

export interface DurabilityOptions {
  platform?: SupportedHostPlatform;
  mode?: number;
  privateDacl?: boolean;
  helper?: RunWindowsHelperOptions;
}

function parentDirectory(platform: SupportedHostPlatform, path: string): string {
  return platform === "win32" ? win32.dirname(path) : dirname(path);
}

async function syncPosixPath(path: string): Promise<void> {
  const handle = await open(path, "r");
  try {
    await handle.sync();
  } finally {
    await handle.close();
  }
}

export async function applyPrivateDacl(path: string, options: DurabilityOptions = {}): Promise<void> {
  const platform = options.platform ?? supportedHostPlatform();
  switch (platform) {
    case "darwin":
    case "linux":
      await chmod(path, options.mode ?? 0o600);
      return;
    case "win32":
      await runWindowsHelper<{ ok: true }>("apply-dacl", [path], options.helper);
      return;
    default:
      return assertNeverPlatform(platform);
  }
}

export async function flushDurablePath(path: string, options: DurabilityOptions = {}): Promise<void> {
  const platform = options.platform ?? supportedHostPlatform();
  switch (platform) {
    case "darwin":
    case "linux":
      await syncPosixPath(path);
      return;
    case "win32":
      await runWindowsHelper<{ ok: true }>("flush-file", [path], options.helper);
      return;
    default:
      return assertNeverPlatform(platform);
  }
}

/** Commit an already-staged file without permitting a copy-based replacement fallback. */
export async function durableReplace(
  source: string,
  destination: string,
  options: DurabilityOptions = {},
): Promise<void> {
  const platform = options.platform ?? supportedHostPlatform();
  await mkdir(parentDirectory(platform, destination), { recursive: true, mode: 0o700 });
  if (options.privateDacl) await applyPrivateDacl(source, { ...options, platform });
  switch (platform) {
    case "darwin":
    case "linux":
      await syncPosixPath(source);
      await rename(source, destination);
      if (options.privateDacl) await chmod(destination, options.mode ?? 0o600);
      await syncPosixPath(parentDirectory(platform, destination));
      return;
    case "win32":
      await runWindowsHelper<{ ok: true }>("replace-file", [source, destination], options.helper);
      if (options.privateDacl) await applyPrivateDacl(destination, { ...options, platform });
      return;
    default:
      return assertNeverPlatform(platform);
  }
}

export async function durableWriteFile(
  path: string,
  data: string | Uint8Array,
  options: DurabilityOptions = {},
): Promise<void> {
  const platform = options.platform ?? supportedHostPlatform();
  const parent = parentDirectory(platform, path);
  await mkdir(parent, { recursive: true, mode: 0o700 });
  const temp = `${path}.tmp-${process.pid}-${randomBytes(8).toString("hex")}`;
  let created = false;
  try {
    const handle = await open(temp, "wx", options.mode ?? 0o600);
    created = true;
    try {
      await handle.writeFile(data);
      await handle.sync();
    } finally {
      await handle.close();
    }
    await durableReplace(temp, path, { ...options, platform });
    created = false;
  } finally {
    if (created) await rm(temp, { force: true }).catch(() => undefined);
  }
}

export async function durableRemove(path: string, options: DurabilityOptions = {}): Promise<void> {
  const platform = options.platform ?? supportedHostPlatform();
  switch (platform) {
    case "darwin":
    case "linux":
      await rm(path, { force: true });
      await syncPosixPath(parentDirectory(platform, path));
      return;
    case "win32":
      await runWindowsHelper<{ ok: true }>("remove-file", [path], options.helper);
      return;
    default:
      return assertNeverPlatform(platform);
  }
}
