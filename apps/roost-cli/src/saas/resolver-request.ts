// Parses bounded tenant-resolver requests and enforces per-client rate limits.
// The resolver calls these routines before touching durable route lookup state.
// Strict parsing keeps malformed and oversized requests on one generic error path.
import { isIP } from "node:net";
import { normalizeAccountEmail } from "@roost/shared/native-credentials";
import {
  JSON_HEADERS,
  MAX_BODY_CHUNKS,
  MAX_HMAC_KEY_BYTES,
  MIN_HMAC_KEY_BYTES,
  TENANT_RESOLVER_MAX_BODY_BYTES,
  TENANT_RESOLVER_MAX_IP_HEADER_BYTES,
} from "./resolver-contract.ts";

interface RateBucket {
  used: number;
  resetAtMs: number;
}

interface RateDecision {
  allowed: boolean;
  retryAfterSeconds: number;
}

export class InvalidResolverRequest extends Error {}

export class BoundedResolverRateLimiter {
  private readonly buckets = new Map<string, RateBucket>();

  constructor(
    private readonly limit: number,
    private readonly windowMs: number,
    private readonly maxBuckets: number,
    private readonly now: () => number,
  ) {
    for (const [name, value] of [
      ["rate limit", limit],
      ["rate window", windowMs],
      ["rate bucket limit", maxBuckets],
    ] as const) {
      if (!Number.isSafeInteger(value) || value <= 0) throw new RangeError(`${name} must be a positive safe integer`);
    }
  }

  consume(key: string): RateDecision {
    const now = this.now();
    if (!Number.isSafeInteger(now) || now < 0) throw new RangeError("resolver clock returned an invalid timestamp");
    let bucket = this.buckets.get(key);
    if (bucket && now >= bucket.resetAtMs) {
      this.buckets.delete(key);
      bucket = undefined;
    }
    if (!bucket) {
      if (this.buckets.size >= this.maxBuckets) {
        for (const [existingKey, existing] of this.buckets) {
          if (now >= existing.resetAtMs) this.buckets.delete(existingKey);
        }
      }
      if (this.buckets.size >= this.maxBuckets) {
        return { allowed: false, retryAfterSeconds: Math.max(1, Math.ceil(this.windowMs / 1_000)) };
      }
      const resetAtMs = now + this.windowMs;
      if (!Number.isSafeInteger(resetAtMs)) throw new RangeError("resolver rate window overflowed");
      bucket = { used: 0, resetAtMs };
      this.buckets.set(key, bucket);
    } else {
      this.buckets.delete(key);
      this.buckets.set(key, bucket);
    }
    if (bucket.used >= this.limit) {
      return {
        allowed: false,
        retryAfterSeconds: Math.max(1, Math.ceil((bucket.resetAtMs - now) / 1_000)),
      };
    }
    bucket.used++;
    return { allowed: true, retryAfterSeconds: 0 };
  }

  clear(): void {
    this.buckets.clear();
  }
}

export function jsonResponse(body: object, status: number, extraHeaders?: Record<string, string>): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { ...JSON_HEADERS, ...extraHeaders },
  });
}

export function invalidRequest(): Response {
  return jsonResponse({ error: "invalid request" }, 400);
}

export function unavailable(): Response {
  return jsonResponse({ error: "request unavailable" }, 503);
}

function canonicalIp(raw: string): string | null {
  const value = raw.trim();
  if (value.length === 0 || Buffer.byteLength(value, "utf8") > 64) return null;
  const version = isIP(value);
  if (version === 4) return value.split(".").map((part) => String(Number(part))).join(".");
  if (version !== 6) return null;
  try {
    const hostname = new URL(`http://[${value}]/`).hostname;
    return hostname.slice(1, -1);
  } catch {
    return null;
  }
}

export function tenantResolverClientIp(request: Request, peerIp: string | null): string | null {
  const forwardedFor = request.headers.get("x-forwarded-for");
  if (forwardedFor !== null) {
    if (
      Buffer.byteLength(forwardedFor, "utf8") > TENANT_RESOLVER_MAX_IP_HEADER_BYTES
      || forwardedFor.includes(",")
    ) {
      return null;
    }
    return canonicalIp(forwardedFor);
  }
  return peerIp === null ? null : canonicalIp(peerIp);
}

export async function readBoundedJson(request: Request): Promise<unknown> {
  const declaredLength = request.headers.get("content-length");
  let expectedLength: number | null = null;
  if (declaredLength !== null) {
    if (!/^(0|[1-9][0-9]*)$/.test(declaredLength)) throw new InvalidResolverRequest();
    expectedLength = Number(declaredLength);
    if (!Number.isSafeInteger(expectedLength) || expectedLength > TENANT_RESOLVER_MAX_BODY_BYTES) {
      throw new InvalidResolverRequest();
    }
  }
  if (request.body === null) throw new InvalidResolverRequest();

  const bytes = new Uint8Array(TENANT_RESOLVER_MAX_BODY_BYTES);
  const reader = request.body.getReader();
  let chunks = 0;
  let length = 0;
  try {
    while (true) {
      const { done, value } = await reader.read();
      if (done) break;
      chunks++;
      if (chunks > MAX_BODY_CHUNKS || length + value.byteLength > bytes.byteLength) {
        throw new InvalidResolverRequest();
      }
      bytes.set(value, length);
      length += value.byteLength;
    }
  } finally {
    reader.releaseLock();
  }
  if (length === 0 || (expectedLength !== null && expectedLength !== length)) throw new InvalidResolverRequest();

  let text: string;
  try {
    text = new TextDecoder("utf-8", { fatal: true }).decode(bytes.subarray(0, length));
    return JSON.parse(text);
  } catch {
    throw new InvalidResolverRequest();
  }
}

export function normalizedEmailFromBody(body: unknown): string {
  if (body === null || typeof body !== "object" || Array.isArray(body)) throw new InvalidResolverRequest();
  const keys = Object.keys(body);
  if (keys.length !== 1 || keys[0] !== "email") throw new InvalidResolverRequest();
  if (!("email" in body)) throw new InvalidResolverRequest();
  const emailRaw = body.email;
  if (typeof emailRaw !== "string") throw new InvalidResolverRequest();
  const email = normalizeAccountEmail(emailRaw);
  if (!email) throw new InvalidResolverRequest();
  return email;
}

export function checkedHmacKey(key: Uint8Array): Buffer {
  if (key.byteLength < MIN_HMAC_KEY_BYTES || key.byteLength > MAX_HMAC_KEY_BYTES) {
    throw new Error("tenant resolver HMAC key must contain between 32 and 1024 bytes");
  }
  return Buffer.from(key);
}
