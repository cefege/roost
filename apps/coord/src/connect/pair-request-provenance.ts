// Pair-request provenance is captured only at the coordinator request boundary.
// It owns trusted-proxy geo headers, bounded user-agent metadata, and the
// deterministic fallback parser used before metadata enters SQLite or the bus.
// CallerOrigin supplies the already-resolved source address and trust decision.

import type { CallerOrigin } from "../middleware/caller-origin.ts";
import { truncatePersistedUtf8 } from "../persistence-input.ts";

export interface PairRequestProvenance {
  readonly userAgent: string | null;
  readonly clientBrowser: string | null;
  readonly clientOs: string | null;
  readonly clientDeviceType: "desktop" | "mobile" | "tablet" | null;
  readonly sourceIp: string;
  readonly countryCode: string | null;
  readonly region: string | null;
  readonly city: string | null;
}

export const MAX_PROVENANCE_UTF8_BYTES = 512;
export const MAX_GEO_UTF8_BYTES = 64;

const CONTROL_CHARACTERS = /[\u0000-\u001f\u007f-\u009f]/g;

type DeviceType = PairRequestProvenance["clientDeviceType"];
type Description = {
  browser: string | null;
  os: string | null;
  deviceType: DeviceType;
};

type NamedPattern = readonly [pattern: RegExp, value: string];

const BROWSER_PATTERNS: readonly NamedPattern[] = [
  [/Edg\//, "Edge"],
  [/OPR\//, "Opera"],
  [/Firefox\//, "Firefox"],
  [/Chrome\//, "Chrome"],
  [/Safari\//, "Safari"],
];

const OS_PATTERNS: readonly NamedPattern[] = [
  [/Windows NT/, "Windows"],
  [/iPhone|iPad/, "iOS"],
  [/Mac OS X/, "macOS"],
  [/Android/, "Android"],
  [/Linux/, "Linux"],
];

const DEVICE_PATTERNS: readonly [pattern: RegExp, value: Exclude<DeviceType, null>][] = [
  [/iPad|Tablet/, "tablet"],
  [/Mobile|iPhone|Android/, "mobile"],
];

/** Capture server-observed pairing metadata without trusting request-body claims. */
export function capturePairRequestProvenance(
  headers: Headers,
  origin: CallerOrigin,
): PairRequestProvenance {
  const userAgentHeader = headers.get("user-agent");
  const userAgent = userAgentHeader === null
    ? null
    : normalizeAndTruncate(userAgentHeader, MAX_PROVENANCE_UTF8_BYTES);
  const description = describeUserAgentWithHints(headers, userAgent ?? "");

  let countryCode: string | null = null;
  let region: string | null = null;
  let city: string | null = null;
  if (origin.listener === "trusted-proxy") {
    const country = readBoundedHeader(headers, "cf-ipcountry", MAX_GEO_UTF8_BYTES);
    countryCode = country === null
      ? null
      : /^[A-Z]{2}$/.test(country.toUpperCase())
        ? country.toUpperCase()
        : null;
    region = readBoundedHeader(headers, "cf-region", MAX_GEO_UTF8_BYTES);
    city = readBoundedHeader(headers, "cf-ipcity", MAX_GEO_UTF8_BYTES);
  }

  return {
    userAgent,
    clientBrowser: description.browser === null
      ? null
      : normalizeAndTruncate(description.browser, MAX_PROVENANCE_UTF8_BYTES),
    clientOs: description.os === null
      ? null
      : normalizeAndTruncate(description.os, MAX_PROVENANCE_UTF8_BYTES),
    clientDeviceType: description.deviceType,
    sourceIp: normalizeAndTruncate(origin.clientIp, MAX_PROVENANCE_UTF8_BYTES),
    countryCode,
    region,
    city,
  };
}

/** Describe a user agent using the ordered compatibility table. */
export function describeUserAgent(userAgent: string): {
  browser: string | null;
  os: string | null;
  deviceType: DeviceType;
} {
  const browser = firstPatternValue(BROWSER_PATTERNS, userAgent);
  const os = firstPatternValue(OS_PATTERNS, userAgent);
  const deviceType = firstPatternValue(DEVICE_PATTERNS, userAgent)
    ?? (browser === null ? null : "desktop");
  return { browser, os, deviceType };
}

function describeUserAgentWithHints(headers: Headers, userAgent: string): Description {
  const fallback = describeUserAgent(userAgent);
  return {
    browser: readBrowserHint(headers) ?? fallback.browser,
    os: readOperatingSystemHint(headers) ?? fallback.os,
    deviceType: readDeviceTypeHint(headers) ?? fallback.deviceType,
  };
}

function readBrowserHint(headers: Headers): string | null {
  const header = headers.get("sec-ch-ua");
  if (header === null || header.length === 0) return null;

  const brands = [...header.matchAll(/"([^\"]+)"/g)].map((match) => match[1]);
  const candidates = brands.length > 0
    ? brands
    : header.split(",").map((brand) => brand.split(";")[0]?.trim() ?? "");
  const usable = candidates.filter((brand) => brand.length > 0 && !/^Not[ _;=]/i.test(brand));
  if (usable.length === 0) return null;

  const knownBrowserPatterns: readonly NamedPattern[] = [
    [/Microsoft Edge/i, "Edge"],
    [/\bEdge\b/i, "Edge"],
    [/Opera/i, "Opera"],
    [/Google Chrome/i, "Chrome"],
    [/Chrome/i, "Chrome"],
    [/Chromium/i, "Chrome"],
    [/Firefox/i, "Firefox"],
    [/Safari/i, "Safari"],
  ];
  return firstPatternValue(knownBrowserPatterns, usable.join(","))
    ?? normalizeAndTruncate(usable[0]!, MAX_PROVENANCE_UTF8_BYTES);
}

function readOperatingSystemHint(headers: Headers): string | null {
  const value = readUnquotedHint(headers, "sec-ch-ua-platform");
  if (value === null) return null;
  const knownOperatingSystems: readonly NamedPattern[] = [
    [/Windows/i, "Windows"],
    [/macOS|Mac OS X/i, "macOS"],
    [/Android/i, "Android"],
    [/iOS|iPhone|iPad/i, "iOS"],
    [/Linux/i, "Linux"],
  ];
  return firstPatternValue(knownOperatingSystems, value) ?? value;
}

function readDeviceTypeHint(headers: Headers): DeviceType {
  const value = headers.get("sec-ch-ua-mobile");
  if (value === null || value.length === 0) return null;
  if (value.trim() === "?1") return "mobile";
  if (value.trim() === "?0") return "desktop";
  return null;
}

function readUnquotedHint(headers: Headers, name: string): string | null {
  const value = headers.get(name);
  if (value === null || value.length === 0) return null;
  const unquoted = value.trim().replace(/^"|"$/g, "");
  return unquoted.length === 0
    ? null
    : normalizeAndTruncate(unquoted, MAX_PROVENANCE_UTF8_BYTES);
}

function readBoundedHeader(
  headers: Headers,
  name: string,
  maxBytes: number,
): string | null {
  const value = headers.get(name);
  return value === null ? null : normalizeAndTruncate(value, maxBytes);
}


function normalizeAndTruncate(value: string, maxBytes: number): string {
  let normalized = "";
  for (let idx = 0; idx < value.length; idx += 1) {
    const isControlCharacter = CONTROL_CHARACTERS.test(value[idx]!);
    CONTROL_CHARACTERS.lastIndex = 0;
    if (isControlCharacter) continue;
    normalized += value[idx];
    if (normalized.length > maxBytes) break;
  }
  return truncatePersistedUtf8(normalized, maxBytes);
}

function firstPatternValue<T extends string>(
  patterns: readonly (readonly [pattern: RegExp, value: T])[],
  input: string,
): T | null {
  return patterns.find(([pattern]) => pattern.test(input))?.[1] ?? null;
}
