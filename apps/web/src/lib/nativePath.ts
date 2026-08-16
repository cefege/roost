// Browser-facing worker path codec. Every route, display, comparison, and
// terminal file-link crosses this module so components never split/join native
// paths themselves. The shared codec owns canonical path math; this adapter
// supplies the worker OS and preserves historical POSIX routes while workers
// hydrate independently from the terminal domain.

import type { SupportedHostPlatform } from "@roost/shared/platform";
import {
  decodeNativePathRoute,
  encodeNativePathRoute,
  nativePathBasename,
  nativePathCrumbs,
  nativePathDirname,
  nativePathIdentityKey,
  nativePathJoin,
  normalizeNativePath,
} from "@roost/shared/native-path";
import { rootStore } from "../store/root.ts";

const WINDOWS_ROUTE_RE = /^~(?:drive|unc)(?:\/|$)/;
const WINDOWS_DRIVE_ABS_RE = /^[A-Za-z]:[\\/]/;
const WINDOWS_DRIVE_REL_RE = /^[A-Za-z]:(?![\\/])/;
const WINDOWS_UNC_RE = /^(?:\\\\|\/\/)[^\\/]+[\\/][^\\/]+/;

export interface WorkerPathCrumb {
  label: string;
  path: string;
}

/** Validate a worker-advertised platform. Unknown values fail closed instead of
 * silently acquiring POSIX or Windows semantics. */
export function supportedWorkerPlatform(value: unknown): SupportedHostPlatform | null {
  switch (value) {
    case "darwin":
    case "linux":
    case "win32":
      return value;
    case null:
    case undefined:
    case "":
      return null;
    default:
      throw new RangeError(`Unsupported worker platform: ${String(value)}`);
  }
}

/** Infer only while the independently-hydrated worker record is absent. Windows
 * canonical paths and tagged routes are unambiguous; all other inputs use the
 * POSIX codec (darwin/linux have identical browser path semantics). */
export function inferNativePathPlatform(pathOrRoute: string): SupportedHostPlatform {
  return WINDOWS_ROUTE_RE.test(pathOrRoute)
    || WINDOWS_DRIVE_ABS_RE.test(pathOrRoute)
    || WINDOWS_UNC_RE.test(pathOrRoute)
    ? "win32"
    : "linux";
}

export function workerPathPlatform(workerFp: string, pathOrRoute = ""): SupportedHostPlatform {
  const declared = supportedWorkerPlatform(rootStore.workers[workerFp]?.os);
  return declared ?? inferNativePathPlatform(pathOrRoute);
}
function pathForCodec(platform: SupportedHostPlatform, path: string): string {
  return platform === "win32" ? path.replace(/\\/g, "/") : path;
}

export function normalizeWorkerPath(workerFp: string, path: string): string {
  const platform = workerPathPlatform(workerFp, path);
  return normalizeNativePath(platform, pathForCodec(platform, path));
}

export function workerPathBasename(workerFp: string, path: string): string {
  const platform = workerPathPlatform(workerFp, path);
  return nativePathBasename(platform, pathForCodec(platform, path));
}

export function workerPathDirname(workerFp: string, path: string): string {
  const platform = workerPathPlatform(workerFp, path);
  return nativePathDirname(platform, pathForCodec(platform, path));
}

export function joinWorkerPath(workerFp: string, base: string, ...parts: string[]): string {
  const platform = workerPathPlatform(workerFp, base);
  const normalizedParts = platform === "win32"
    ? parts.map((part) => pathForCodec(platform, part))
    : parts;
  return nativePathJoin(platform, pathForCodec(platform, base), ...normalizedParts);
}

export function workerPathCrumbs(workerFp: string, path: string): WorkerPathCrumb[] {
  const platform = workerPathPlatform(workerFp, path);
  return nativePathCrumbs(platform, pathForCodec(platform, path))
    .map((crumb) => ({ label: crumb.name, path: crumb.path }));
}

export function workerPathIdentity(workerFp: string, path: string): string {
  const platform = workerPathPlatform(workerFp, path);
  return nativePathIdentityKey(platform, pathForCodec(platform, path));
}

export function sameWorkerPath(workerFp: string, left: string, right: string): boolean {
  const platform = workerPathPlatform(workerFp, left || right);
  return nativePathIdentityKey(platform, pathForCodec(platform, left))
    === nativePathIdentityKey(platform, pathForCodec(platform, right));
}

export function encodeWorkerPathRoute(workerFp: string, path: string): string {
  const platform = workerPathPlatform(workerFp, path);
  return encodeNativePathRoute(platform, pathForCodec(platform, path));
}

export function decodeWorkerPathRoute(workerFp: string, route: string): string {
  return decodeNativePathRoute(workerPathPlatform(workerFp, route), route);
}

/** Compact path label used by tabs/sidebar. POSIX output is intentionally the
 * historical user/basename form; Windows mirrors it for C:/Users/<user>/… and
 * otherwise shows the native basename (including a drive/share root). */
export function shortWorkerPath(workerFp: string, path: string): string {
  const crumbs = workerPathCrumbs(workerFp, path);
  if (crumbs.length === 0) return path;
  const names = crumbs.map((crumb) => crumb.label);
  const last = names[names.length - 1] ?? path;
  const userRoot = names.findIndex((name) => name.toLowerCase() === "users" || name.toLowerCase() === "home");
  const user = userRoot >= 0 && userRoot + 1 < names.length ? names[userRoot + 1] : null;
  if (!user || user === last) return last;
  return `${user}/${last}`;
}

/** Resolve a terminal-emitted absolute/relative path against the session cwd.
 * Windows backslashes are accepted at this browser boundary and converted to
 * the canonical forward-slash worker form. Drive-relative paths (C:foo) remain
 * rejected because they depend on hidden per-drive shell state. */
export function resolveWorkerPath(workerFp: string, cwd: string, rawPath: string): string | null {
  if (!cwd || !rawPath) return null;
  const platform = workerPathPlatform(workerFp, cwd || rawPath);
  const raw = platform === "win32" ? rawPath.replace(/\\/g, "/") : rawPath;

  if (platform === "win32") {
    if (WINDOWS_DRIVE_REL_RE.test(raw)) return null;
    if (WINDOWS_DRIVE_ABS_RE.test(raw) || WINDOWS_UNC_RE.test(raw)) {
      return normalizeNativePath(platform, raw);
    }
    // A rooted path without a drive is rooted on the cwd's current drive.
    if (raw.startsWith("/")) {
      const drive = cwd.match(/^([A-Za-z]:)[\\/]/)?.[1];
      if (!drive) return null;
      return normalizeNativePath(platform, `${drive}${raw}`);
    }
  } else if (raw.startsWith("/")) {
    return normalizeNativePath(platform, raw);
  }

  if (raw === "~" || raw.startsWith("~/")) {
    const crumbs = nativePathCrumbs(platform, cwd);
    const homeRoot = crumbs.findIndex((crumb) => {
      const name = crumb.name.toLowerCase();
      return name === "users" || name === "home";
    });
    const home = homeRoot >= 0 ? crumbs[homeRoot + 1]?.path : undefined;
    if (!home) return null;
    return raw === "~" ? home : nativePathJoin(platform, home, raw.slice(2));
  }

  return nativePathJoin(platform, cwd, raw.replace(/^\.\//, ""));
}

export function workerFileHref(
  workerFp: string,
  path: string,
  line: number | null = null,
): string {
  const route = encodeWorkerPathRoute(workerFp, path);
  return `/file/${encodeURIComponent(workerFp)}/${route}${line ? `#L${line}` : ""}`;
}

/** Invert workerFileHref. The route tag is sufficient to decode a Windows deep
 * link before the workers domain hydrates. */
export function parseWorkerFileHref(href: string): { workerFp: string; path: string } | null {
  const noHash = href.replace(/#L\d+$/, "");
  const match = noHash.match(/^\/file\/([^/]+)\/(.+)$/);
  if (!match) return null;
  try {
    const workerFp = decodeURIComponent(match[1]!);
    return { workerFp, path: decodeWorkerPathRoute(workerFp, match[2]!) };
  } catch {
    return null;
  }
}
