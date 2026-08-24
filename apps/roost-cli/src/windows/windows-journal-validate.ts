// Generic value validators shared by the Windows update-journal parser and
// the signed release/current manifest parsers: JSON/record narrowing plus
// one checked constructor per wire scalar (digests, builds, sizes,
// timestamps, URLs, relative/absolute paths, versions).
//
// Callers: windows-update-journal.ts (journal assertions) and
// windows-release-manifest.ts (manifest parsing). Every check throws with a
// `label`-prefixed message so a rejected journal names the exact field.
import { isAbsolute, posix, win32 } from "node:path";

export const SHA256_RE = /^[0-9a-f]{64}$/;
export const BUILD_ID_RE = /^[0-9a-f]{40,64}$/;

export function parseJson(raw: string, label: string): unknown {
  try {
    return JSON.parse(raw);
  } catch (error) {
    throw new Error(`invalid ${label} JSON: ${String(error)}`);
  }
}

export function record(value: unknown, label: string): Record<string, unknown> {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new Error(`${label} must be an object`);
  }
  return value as Record<string, unknown>;
}

export function nonempty(value: unknown, label: string): string {
  if (typeof value !== "string" || value.length === 0 || value.includes("\0")) {
    throw new Error(`${label} must be non-empty`);
  }
  return value;
}

export function sha(value: unknown, label: string): string {
  if (typeof value !== "string" || !SHA256_RE.test(value)) {
    throw new Error(`${label} must be lowercase SHA-256`);
  }
  return value;
}

export function buildIdentity(value: unknown, label: string): string {
  if (typeof value !== "string" || !BUILD_ID_RE.test(value)) {
    throw new Error(`${label} must be a lowercase immutable build id`);
  }
  return value;
}

export function fileSize(value: unknown, label: string): number {
  if (!Number.isSafeInteger(value) || (value as number) < 0) {
    throw new Error(`${label} must be a non-negative safe integer`);
  }
  return value as number;
}

export function isoTimestamp(value: unknown, label: string): string {
  const timestamp = nonempty(value, label);
  const parsed = Date.parse(timestamp);
  if (!Number.isFinite(parsed) || new Date(parsed).toISOString() !== timestamp) {
    throw new Error(`${label} must be an ISO-8601 UTC timestamp`);
  }
  return timestamp;
}

export function httpsUrl(value: unknown, label: string): string {
  const text = nonempty(value, label);
  let url: URL;
  try {
    url = new URL(text);
  } catch {
    throw new Error(`${label} must be absolute`);
  }
  if (url.protocol !== "https:") throw new Error(`${label} must use HTTPS`);
  return text;
}

export function safeRelative(value: unknown, label: string): string {
  const path = nonempty(value, label).replaceAll("\\", "/");
  if (
    isAbsolute(path)
    || win32.isAbsolute(path)
    || posix.isAbsolute(path)
    || path.startsWith("/")
    || /^[A-Za-z]:/.test(path)
    || path.split("/").some((part) => !part || part === "." || part === "..")
  ) {
    throw new Error(`${label} is unsafe`);
  }
  return path;
}

export function absolutePath(value: unknown, label: string): string {
  const path = nonempty(value, label);
  if (!isAbsolute(path) && !win32.isAbsolute(path) && !posix.isAbsolute(path)) {
    throw new Error(`${label} must be absolute`);
  }
  return path;
}

export function releaseVersion(value: unknown, label: string): string {
  const version = nonempty(value, label);
  if (/[/\\]/.test(version) || version === "." || version === "..") {
    throw new Error(`${label} is unsafe`);
  }
  return version;
}
