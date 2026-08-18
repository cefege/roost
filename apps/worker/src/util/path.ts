import { realpathSync } from "node:fs";
import { homedir } from "node:os";
import { win32 } from "node:path";
import {
  assertNeverPlatform,
  supportedHostPlatform,
  type SupportedHostPlatform,
} from "@roost/shared/platform";

const WINDOWS_DRIVE_RELATIVE = /^[A-Za-z]:(?![\\/])/;
const WINDOWS_DRIVE_ABSOLUTE = /^[A-Za-z]:[\\/]/;
const WINDOWS_UNC_ABSOLUTE = /^(?:\\\\|\/\/)[^\\/]+[\\/][^\\/]+(?:[\\/]|$)/;

function decodePathComponent(value: string): string {
  try {
    return decodeURIComponent(value);
  } catch {
    return value;
  }
}

/**
 * Convert a worker path to its wire/display representation. POSIX is left
 * byte-for-byte unchanged. Windows is case-preserving, uses forward slashes,
 * and never admits drive-relative paths (`C:foo`).
 */
export function normalizeWorkerPath(
  value: string,
  platform: SupportedHostPlatform = supportedHostPlatform(),
): string {
  switch (platform) {
    case "darwin":
    case "linux":
      return value;
    case "win32": {
      if (WINDOWS_DRIVE_RELATIVE.test(value)) {
        throw new Error(`drive-relative Windows path is not supported: ${value}`);
      }
      let native = value.replace(/\//g, "\\");
      if (native.startsWith("\\\\?\\UNC\\")) native = `\\\\${native.slice(8)}`;
      else if (native.startsWith("\\\\?\\")) native = native.slice(4);
      const normalized = win32.normalize(native).replace(/\\/g, "/");
      if (WINDOWS_DRIVE_ABSOLUTE.test(normalized)) {
        return `${normalized[0]!.toUpperCase()}${normalized.slice(1)}`;
      }
      return normalized;
    }
    default:
      return assertNeverPlatform(platform);
  }
}

/** Convert the canonical worker representation at an OS API boundary. */
export function toFilesystemPath(
  value: string,
  platform: SupportedHostPlatform = supportedHostPlatform(),
): string {
  switch (platform) {
    case "darwin":
    case "linux":
      return value;
    case "win32":
      return win32.normalize(normalizeWorkerPath(value, platform).replace(/\//g, "\\"));
    default:
      return assertNeverPlatform(platform);
  }
}

/** Realpath an existing path and return the canonical worker representation. */
export function canonicalExistingWorkerPath(
  value: string,
  platform: SupportedHostPlatform = supportedHostPlatform(),
): string {
  return normalizeWorkerPath(realpathSync.native(toFilesystemPath(value, platform)), platform);
}

/** Case-folded only on Windows, for identity/containment comparisons. */
export function workerPathIdentityKey(
  value: string,
  platform: SupportedHostPlatform = supportedHostPlatform(),
): string {
  const normalized = normalizeWorkerPath(value, platform);
  return platform === "win32" ? normalized.toLocaleLowerCase("en-US") : normalized;
}

export function isWorkerPathWithin(
  base: string,
  candidate: string,
  platform: SupportedHostPlatform = supportedHostPlatform(),
): boolean {
  const baseKey = workerPathIdentityKey(base, platform).replace(/\/+$/, "");
  const candidateKey = workerPathIdentityKey(candidate, platform).replace(/\/+$/, "");
  return candidateKey === baseKey || candidateKey.startsWith(`${baseKey}/`);
}

// Expand a leading `~` to the user's home dir. POSIX keeps its historical
// `~/` behavior; Windows accepts both slash styles and emits canonical `/`.
export function expandTilde(
  value: string,
  platform: SupportedHostPlatform = supportedHostPlatform(),
  home = homedir(),
): string {
  switch (platform) {
    case "darwin":
    case "linux":
      if (value === "~") return home;
      if (value.startsWith("~/")) return home + value.slice(1);
      return value;
    case "win32": {
      const normalizedHome = normalizeWorkerPath(home, platform);
      if (value === "~") return normalizedHome;
      if (value.startsWith("~/") || value.startsWith("~\\")) {
        return normalizeWorkerPath(`${normalizedHome}/${value.slice(2)}`, platform);
      }
      return normalizeWorkerPath(value, platform);
    }
    default:
      return assertNeverPlatform(platform);
  }
}

/** Parse an OSC 7 file URL into the worker's canonical cwd representation. */
export function parseOsc7WorkerPath(
  raw: string,
  platform: SupportedHostPlatform = supportedHostPlatform(),
): string | null {
  const match = /^file:\/\/([^/]*)(\/.*)$/.exec(raw);
  if (!match) return null;
  const host = decodePathComponent(match[1] ?? "");
  const decodedPath = decodePathComponent(match[2] ?? "");
  if (decodedPath.includes("\0") || host.includes("\0")) return null;
  switch (platform) {
    case "darwin":
    case "linux":
      return decodedPath;
    case "win32": {
      if (host) {
        const unc = normalizeWorkerPath(`//${host}${decodedPath}`, platform);
        return WINDOWS_UNC_ABSOLUTE.test(unc) ? unc : null;
      }
      const drivePath = /^\/([A-Za-z]:\/.*)$/.exec(decodedPath)?.[1];
      if (drivePath) return normalizeWorkerPath(drivePath, platform);
      const unc = normalizeWorkerPath(decodedPath, platform);
      return WINDOWS_UNC_ABSOLUTE.test(unc) ? unc : null;
    }
    default:
      return assertNeverPlatform(platform);
  }
}
