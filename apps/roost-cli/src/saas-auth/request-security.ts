/**
 * Enforces shared HTTP request, body, origin, cookie, and client-address constraints.
 * Every public signup gateway protocol uses these routines before parsing attacker input.
 * Central limits and exact-object checks prevent routes from drifting to weaker validation.
 */

import { isIP } from "node:net";
import { parseStrictJson } from "./canonical-json.ts";
import { GATEWAY_PUBLIC_ORIGIN } from "./gateway-config.ts";

export const GATEWAY_MAX_BODY_BYTES = 16 * 1024;
export const GATEWAY_MAX_BODY_CHUNKS = 32;
export const GATEWAY_MAX_IP_HEADER_BYTES = 64;

export const GATEWAY_SECURITY_HEADERS = Object.freeze({
  "cache-control": "no-store",
  "content-security-policy": "default-src 'none'; frame-ancestors 'none'; base-uri 'none'",
  "referrer-policy": "no-referrer",
  "x-content-type-options": "nosniff",
  "x-frame-options": "DENY",
} as const);

export class InvalidGatewayRequest extends Error {
  constructor() {
    super("invalid gateway request");
    this.name = "InvalidGatewayRequest";
  }
}

export function gatewayJson(
  body: object,
  status = 200,
  extraHeaders?: HeadersInit,
): Response {
  const headers = new Headers(GATEWAY_SECURITY_HEADERS);
  headers.set("content-type", "application/json; charset=utf-8");
  if (extraHeaders) {
    const additions = new Headers(extraHeaders);
    additions.forEach((value, name) => headers.append(name, value));
  }
  return new Response(JSON.stringify(body), { status, headers });
}

export function gatewayRedirect(location: string, cookies: readonly string[] = []): Response {
  const headers = new Headers(GATEWAY_SECURITY_HEADERS);
  headers.set("location", location);
  for (const cookie of cookies) headers.append("set-cookie", cookie);
  return new Response(null, { status: 303, headers });
}

export function gatewayNotFound(): Response {
  return gatewayJson({ error: "not found" }, 404);
}

export function gatewayInvalid(): Response {
  return gatewayJson({ error: "invalid request" }, 400);
}

export function gatewayUnavailable(): Response {
  return gatewayJson({ error: "request unavailable" }, 503);
}

function canonicalIp(raw: string): string | null {
  if (raw !== raw.trim() || raw.length === 0 || Buffer.byteLength(raw, "utf8") > GATEWAY_MAX_IP_HEADER_BYTES) {
    return null;
  }
  const version = isIP(raw);
  if (version === 4) return raw.split(".").map((part) => String(Number(part))).join(".");
  if (version !== 6) return null;
  try {
    const hostname = new URL(`http://[${raw}]/`).hostname;
    return hostname.slice(1, -1).toLowerCase();
  } catch {
    return null;
  }
}

/** The only trusted caller is the root loopback bridge. It overwrites XFF from
 * one Caddy-validated CF-Connecting-IP value; an appended chain is refused. */
export function gatewayClientIp(request: Request, peerIp: string | null): string | null {
  if (peerIp !== "127.0.0.1") return null;
  const forwarded = request.headers.get("x-forwarded-for");
  if (forwarded === null || forwarded.includes(",")) return null;
  const clientIp = canonicalIp(forwarded);
  if (clientIp === null) return null;
  const cloudflareIp = request.headers.get("cf-connecting-ip");
  if (cloudflareIp !== null) {
    if (cloudflareIp.includes(",") || canonicalIp(cloudflareIp) !== clientIp) return null;
  }
  return clientIp;
}

export function requireBrowserPost(request: Request, peerIp: string | null): string {
  const clientIp = gatewayClientIp(request, peerIp);
  if (
    clientIp === null
    || request.headers.get("origin") !== GATEWAY_PUBLIC_ORIGIN
    || request.headers.get("content-type") !== "application/json"
    || request.headers.has("content-encoding")
  ) throw new InvalidGatewayRequest();
  return clientIp;
}

export function requireGatewayGet(request: Request, peerIp: string | null): string {
  const clientIp = gatewayClientIp(request, peerIp);
  if (clientIp === null) throw new InvalidGatewayRequest();
  return clientIp;
}

export async function readBoundedJson(
  request: Request,
  maxBytes = GATEWAY_MAX_BODY_BYTES,
): Promise<unknown> {
  if (!Number.isSafeInteger(maxBytes) || maxBytes <= 0 || maxBytes > GATEWAY_MAX_BODY_BYTES) {
    throw new InvalidGatewayRequest();
  }
  const declared = request.headers.get("content-length");
  let expected: number | null = null;
  if (declared !== null) {
    if (!/^(0|[1-9][0-9]*)$/u.test(declared)) throw new InvalidGatewayRequest();
    expected = Number(declared);
    if (!Number.isSafeInteger(expected) || expected <= 0 || expected > maxBytes) {
      throw new InvalidGatewayRequest();
    }
  }
  if (request.body === null) throw new InvalidGatewayRequest();
  const bytes = new Uint8Array(maxBytes);
  const reader = request.body.getReader();
  let chunks = 0;
  let length = 0;
  try {
    while (true) {
      const { done, value } = await reader.read();
      if (done) break;
      chunks++;
      if (chunks > GATEWAY_MAX_BODY_CHUNKS || length + value.byteLength > maxBytes) {
        throw new InvalidGatewayRequest();
      }
      bytes.set(value, length);
      length += value.byteLength;
    }
  } finally {
    reader.releaseLock();
  }
  if (length === 0 || (expected !== null && expected !== length)) throw new InvalidGatewayRequest();
  try {
    const text = new TextDecoder("utf-8", { fatal: true }).decode(bytes.subarray(0, length));
    return parseStrictJson(text);
  } catch {
    throw new InvalidGatewayRequest();
  }
}

export function exactObject(value: unknown, fields: readonly string[]): Record<string, unknown> {
  if (value === null || typeof value !== "object" || Array.isArray(value)) throw new InvalidGatewayRequest();
  const prototype = Object.getPrototypeOf(value);
  if (prototype !== Object.prototype && prototype !== null) throw new InvalidGatewayRequest();
  const object = value as Record<string, unknown>;
  const actual = Object.keys(object).sort();
  const expected = [...fields].sort();
  if (actual.length !== expected.length || actual.some((key, index) => key !== expected[index])) {
    throw new InvalidGatewayRequest();
  }
  return object;
}

export function boundedText(value: unknown, maxBytes: number): string {
  if (
    typeof value !== "string"
    || value.length === 0
    || Buffer.byteLength(value, "utf8") > maxBytes
    || /[\u0000-\u001f\u007f]/u.test(value)
  ) throw new InvalidGatewayRequest();
  return value;
}

export function requestCookie(request: Request, name: string): string | null {
  const raw = request.headers.get("cookie");
  if (raw === null || Buffer.byteLength(raw, "utf8") > 8_192) return null;
  let found: string | null = null;
  for (const segment of raw.split(";")) {
    const separator = segment.indexOf("=");
    if (separator <= 0) continue;
    const key = segment.slice(0, separator).trim();
    if (key !== name) continue;
    const value = segment.slice(separator + 1).trim();
    if (found !== null || !/^[A-Za-z0-9_-]{22,256}$/u.test(value)) return null;
    found = value;
  }
  return found;
}

export function secureCookie(name: string, value: string, maxAgeSeconds?: number): string {
  const age = maxAgeSeconds === undefined ? "" : `; Max-Age=${maxAgeSeconds}`;
  return `${name}=${value}; Secure; HttpOnly; SameSite=Lax; Path=/${age}`;
}

export function expireCookie(name: string): string {
  return `${name}=; Secure; HttpOnly; SameSite=Lax; Path=/; Max-Age=0`;
}
