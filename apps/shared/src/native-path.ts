import type { SupportedHostPlatform } from "./platform.ts";
import { assertNeverPlatform, isSupportedHostPlatform } from "./platform.ts";

export interface NativePathCrumb {
  name: string;
  path: string;
}

function rejectInvalidPath(path: string): void {
  if (!path || path.includes("\0")) throw new Error("native path must be non-empty and contain no NUL bytes");
}

function normalizeSegments(segments: string[], floor: number): string[] {
  const out = segments.slice(0, floor);
  for (const segment of segments.slice(floor)) {
    if (!segment || segment === ".") continue;
    if (segment === "..") {
      if (out.length === floor) throw new Error("native path escapes its root");
      out.pop();
      continue;
    }
    out.push(segment);
  }
  return out;
}

function normalizeHomeSentinel(input: string): string | null {
  const path = input.replaceAll("\\", "/");
  if (path !== "~" && !path.startsWith("~/")) return null;
  const segments = normalizeSegments(path.split("/"), 1);
  return segments.length === 1 ? "~" : `~/${segments.slice(1).join("/")}`;
}

/** Canonical lexical worker path. Existing-path realpath resolution happens at the worker boundary. */
export function normalizeNativePath(platform: SupportedHostPlatform, input: string): string {
  rejectInvalidPath(input);
  const homeSentinel = normalizeHomeSentinel(input);
  if (homeSentinel) return homeSentinel;
  switch (platform) {
    case "darwin":
    case "linux": {
      if (!input.startsWith("/")) throw new Error(`POSIX worker path must be absolute: ${input}`);
      const normalized = normalizeSegments(input.split("/"), 1);
      return normalized.length === 1 ? "/" : `/${normalized.slice(1).join("/")}`;
    }
    case "win32": {
      const path = input.replaceAll("\\", "/");
      const drive = /^([A-Za-z]):(?:\/(.*))?$/.exec(path);
      if (drive) {
        const letter = drive[1]!.toUpperCase();
        const tail = normalizeSegments((drive[2] ?? "").split("/"), 0);
        return tail.length ? `${letter}:/${tail.join("/")}` : `${letter}:/`;
      }
      if (/^[A-Za-z]:/.test(path)) throw new Error(`drive-relative Windows path is not allowed: ${input}`);
      if (path.startsWith("//")) {
        const parts = path.slice(2).split("/");
        if (!parts[0] || !parts[1]) throw new Error(`UNC path requires server and share: ${input}`);
        const normalized = normalizeSegments(parts, 2);
        return `//${normalized.join("/")}`;
      }
      throw new Error(`Windows worker path must be drive-absolute or UNC: ${input}`);
    }
    default:
      return assertNeverPlatform(platform);
  }
}

/** Fold the identity differences the host filesystem itself treats as one path:
 *  Windows case, and macOS's three system symlinks. `/tmp`, `/var` and `/etc`
 *  ARE `/private/{tmp,var,etc}` on darwin, so one directory otherwise gets two
 *  keys depending on whether the value came from what a user typed or from a
 *  realpath'd source (a session's cwd, which the shell reports via OSC 7).
 *  Folding only these three is exact, not a heuristic: any other `/private/x`
 *  is a real directory and must stay distinct. */
const DARWIN_PRIVATE_ROOTS = ["tmp", "var", "etc"] as const;

export function nativePathIdentityKey(platform: SupportedHostPlatform, path: string): string {
  const normalized = normalizeNativePath(platform, path);
  switch (platform) {
    case "linux": return normalized;
    case "darwin": {
      for (const root of DARWIN_PRIVATE_ROOTS) {
        if (normalized === `/${root}` || normalized.startsWith(`/${root}/`)) return `/private${normalized}`;
      }
      return normalized;
    }
    case "win32": return normalized.toLocaleLowerCase("en-US");
    default: return assertNeverPlatform(platform);
  }
}

/** Same-directory test for a worker-advertised `os` string, for callers that
 *  hold a raw os column rather than a validated platform (coord's workspace
 *  dedupe). An unknown os, or a path either side cannot normalize (relative,
 *  drive-relative), falls back to exact equality — never a false merge. */
export function sameWorkerFolder(os: unknown, left: string, right: string): boolean {
  if (!isSupportedHostPlatform(os)) return left === right;
  try {
    return nativePathIdentityKey(os, left) === nativePathIdentityKey(os, right);
  } catch {
    return left === right;
  }
}

export function nativePathToFsPath(platform: SupportedHostPlatform, path: string): string {
  const normalized = normalizeNativePath(platform, path);
  if (normalized === "~" || normalized.startsWith("~/")) {
    throw new Error("the browse home sentinel must be resolved before a filesystem operation");
  }
  switch (platform) {
    case "darwin":
    case "linux": return normalized;
    case "win32": return normalized.replaceAll("/", "\\");
    default: return assertNeverPlatform(platform);
  }
}

export function nativePathBasename(platform: SupportedHostPlatform, path: string): string {
  rejectInvalidPath(path);
  const isAbsolute = path.startsWith("/")
    || path === "~"
    || path.startsWith("~/")
    || (platform === "win32" && (/^[A-Za-z]:[\\/]/.test(path) || path.startsWith("\\\\")));
  if (!isAbsolute) {
    const token = path.replaceAll("\\", "/").replace(/\/+$/, "");
    return token.split("/").at(-1) ?? token;
  }
  const normalized = normalizeNativePath(platform, path);
  if (normalized === "~") return "~";
  if (normalized === "/") return "/";
  if (platform === "win32" && /^[A-Z]:\/$/.test(normalized)) return normalized.slice(0, 2);
  const parts = normalized.split("/");
  return parts.at(-1) ?? normalized;
}

export function nativePathDirname(platform: SupportedHostPlatform, path: string): string {
  const normalized = normalizeNativePath(platform, path);
  if (normalized === "~") return "~";
  if (normalized.startsWith("~/") && normalized.lastIndexOf("/") === 1) return "~";
  if (normalized === "/" || (platform === "win32" && /^[A-Z]:\/$/.test(normalized))) return normalized;
  if (platform === "win32" && normalized.startsWith("//")) {
    const parts = normalized.slice(2).split("/");
    if (parts.length <= 2) return normalized;
    parts.pop();
    return `//${parts.join("/")}`;
  }
  const slash = normalized.lastIndexOf("/");
  if (platform === "win32" && slash === 2) return normalized.slice(0, 3);
  return slash <= 0 ? "/" : normalized.slice(0, slash);
}

export function nativePathJoin(platform: SupportedHostPlatform, base: string, ...parts: string[]): string {
  let joined = normalizeNativePath(platform, base);
  for (const part of parts) {
    rejectInvalidPath(part);
    const clean = part.replaceAll("\\", "/");
    if (clean.startsWith("/") || /^[A-Za-z]:/.test(clean)) {
      throw new Error(`nativePathJoin only accepts relative child parts: ${part}`);
    }
    joined = `${joined.replace(/\/$/, "")}/${clean}`;
  }
  return normalizeNativePath(platform, joined);
}

export function nativePathCrumbs(platform: SupportedHostPlatform, path: string): NativePathCrumb[] {
  const normalized = normalizeNativePath(platform, path);
  if (normalized === "~" || normalized.startsWith("~/")) {
    const crumbs: NativePathCrumb[] = [{ name: "~", path: "~" }];
    let current = "~";
    for (const segment of normalized.slice(2).split("/").filter(Boolean)) {
      current += `/${segment}`;
      crumbs.push({ name: segment, path: current });
    }
    return crumbs;
  }
  if (platform === "win32") {
    const drive = /^([A-Z]:)\/(.*)$/.exec(normalized);
    if (drive) {
      const crumbs: NativePathCrumb[] = [{ name: drive[1]!, path: `${drive[1]}/` }];
      let current = `${drive[1]}/`;
      for (const segment of drive[2]!.split("/").filter(Boolean)) {
        current = `${current.replace(/\/$/, "")}/${segment}`;
        crumbs.push({ name: segment, path: current });
      }
      return crumbs;
    }
    const parts = normalized.slice(2).split("/");
    const root = `//${parts[0]}/${parts[1]}`;
    const crumbs: NativePathCrumb[] = [{ name: root, path: root }];
    let current = root;
    for (const segment of parts.slice(2)) {
      current += `/${segment}`;
      crumbs.push({ name: segment, path: current });
    }
    return crumbs;
  }
  const crumbs: NativePathCrumb[] = [{ name: "/", path: "/" }];
  let current = "";
  for (const segment of normalized.split("/").filter(Boolean)) {
    current += `/${segment}`;
    crumbs.push({ name: segment, path: current });
  }
  return crumbs;
}

function encodeSegments(segments: string[]): string {
  return segments.map((segment) => encodeURIComponent(segment)).join("/");
}

/** POSIX encoding is byte-for-byte compatible with the historical route codec. */
export function encodeNativePathRoute(platform: SupportedHostPlatform, path: string): string {
  rejectInvalidPath(path);
  switch (platform) {
    case "darwin":
    case "linux":
      if (!path.startsWith("/")) throw new Error(`POSIX worker path must be absolute: ${path}`);
      return path.split("/").map((segment) => segment ? encodeURIComponent(segment) : segment).join("/").replace(/^\//, "");
    case "win32": {
      const normalized = normalizeNativePath(platform, path);
      if (normalized === "~" || normalized.startsWith("~/")) {
        throw new Error("the browse home sentinel must be resolved before route encoding");
      }
      const drive = /^([A-Z]):\/(.*)$/.exec(normalized);
      if (drive) return `~drive/${drive[1]}/${encodeSegments(drive[2]!.split("/").filter(Boolean))}`.replace(/\/$/, "");
      const parts = normalized.slice(2).split("/");
      return `~unc/${encodeSegments(parts)}`;
    }
    default:
      return assertNeverPlatform(platform);
  }
}

function decodeSegments(route: string): string[] {
  return route.split("/").filter(Boolean).map((segment) => {
    const decoded = decodeURIComponent(segment);
    if (!decoded || decoded.includes("/") || decoded.includes("\\") || decoded.includes("\0")) {
      throw new Error("invalid encoded native path segment");
    }
    return decoded;
  });
}

export function decodeNativePathRoute(platform: SupportedHostPlatform, route: string): string {
  switch (platform) {
    case "darwin":
    case "linux": {
      const inner = route.split("/").map((segment) => segment ? decodeURIComponent(segment) : segment).join("/").replace(/^\//, "");
      const decoded = `/${inner}`;
      rejectInvalidPath(decoded);
      return decoded;
    }
    case "win32": {
      const parts = decodeSegments(route);
      const tag = parts.shift();
      if (tag === "~drive") {
        const drive = parts.shift();
        if (!drive || !/^[A-Za-z]$/.test(drive)) throw new Error("invalid Windows drive route");
        return normalizeNativePath(platform, `${drive}:/${parts.join("/")}`);
      }
      if (tag === "~unc") {
        if (parts.length < 2) throw new Error("invalid Windows UNC route");
        return normalizeNativePath(platform, `//${parts.join("/")}`);
      }
      throw new Error("Windows native path route is missing a tagged root");
    }
    default:
      return assertNeverPlatform(platform);
  }
}
