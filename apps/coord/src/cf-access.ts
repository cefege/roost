// Verifies Cloudflare Access assertions at the coordinator front door.
// The fetch handler and pairing RPC share one gate built from CoordConfig.
// JWKS material is cached here; malformed or unverifiable assertions fail closed.

import { signal } from "@roost/shared/diag";
import type { CoordConfig } from "@roost/shared/config";
import { b64urlDecode } from "@roost/shared/jwt-base";
import { log } from "@roost/shared/log";

export interface CloudflareAccessIdentity {
  readonly email: string;
  readonly subject: string;
}

export interface CloudflareAccessGate {
  /** Verified identity, or null when the assertion is absent or invalid. */
  verify(headers: Headers): Promise<CloudflareAccessIdentity | null>;
}

export const ACCESS_CLOCK_SKEW_MS = 60_000;
export const JWKS_TTL_MS = 15 * 60_000;
export const JWKS_REFETCH_MIN_INTERVAL_MS = 60_000;
export const MAX_ACCESS_EMAIL_UTF8_BYTES = 320;

const BASE64URL_RE = /^[A-Za-z0-9_-]+$/;
type RejectionReason =
  | "absent"
  | "malformed"
  | "unknown_kid"
  | "bad_signature"
  | "bad_issuer"
  | "bad_audience"
  | "expired"
  | "bad_claims";

interface AccessJwtHeader {
  readonly alg?: unknown;
  readonly kid?: unknown;
}

interface ParsedAssertion {
  readonly headerPart: string;
  readonly payloadPart: string;
  readonly signature: Uint8Array;
  readonly payload: Uint8Array;
  readonly kid: string;
}

interface JwksCache {
  readonly fetchedAtMs: number;
  readonly keys: Map<string, JsonWebKey>;
}

/** Build the verifier, or leave every surface unchanged when Access is off. */
export function createCloudflareAccessGate(cfg: CoordConfig): CloudflareAccessGate | null {
  const teamDomain = cfg.cfAccessTeamDomain;
  const audience = cfg.cfAccessAud;
  if (!teamDomain || !audience) return null;

  const issuer = `https://${teamDomain}`;
  const jwksUrl = `${issuer}/cdn-cgi/access/certs`;
  let jwksCache: JwksCache | null = null;
  let lastUnknownKidRefetchAt = Number.NEGATIVE_INFINITY;
  let jwksFetchInFlight: Promise<boolean> | null = null;

  const fetchJwks = async (): Promise<boolean> => {
    if (jwksFetchInFlight) return jwksFetchInFlight;

    const fetchPromise = Promise.resolve()
      .then(() => fetch(jwksUrl))
      .then(async (response) => {
        if (!response.ok) {
          throw new Error(`JWKS fetch returned HTTP ${response.status}`);
        }
        const body = await response.json() as unknown;
        const keys = parseJwks(body);
        if (keys === null) throw new Error("JWKS response did not contain a keys array");
        jwksCache = { fetchedAtMs: Date.now(), keys };
        return true;
      })
      .catch((error: unknown) => {
        log.warn("cf-access", "jwks_fetch_failed", { error });
        return false;
      })
      .finally(() => {
        jwksFetchInFlight = null;
      });
    jwksFetchInFlight = fetchPromise;
    return fetchPromise;
  };

  const findJwk = async (kid: string): Promise<JsonWebKey | null> => {
    const now = Date.now();
    let refreshed = false;
    if (jwksCache === null || now - jwksCache.fetchedAtMs >= JWKS_TTL_MS) {
      if (!await fetchJwks()) return null;
      refreshed = true;
    }

    let key = jwksCache?.keys.get(kid) ?? null;
    if (key !== null || refreshed) return key;

    if (now - lastUnknownKidRefetchAt < JWKS_REFETCH_MIN_INTERVAL_MS) return null;
    lastUnknownKidRefetchAt = now;
    if (!await fetchJwks()) return null;
    key = jwksCache?.keys.get(kid) ?? null;
    return key;
  };

  const verify = async (headers: Headers): Promise<CloudflareAccessIdentity | null> => {
    const assertion = headers.get("cf-access-jwt-assertion");
    if (assertion === null) return rejectAccess("absent");

    const parsed = parseAssertion(assertion);
    if (parsed === null) return rejectAccess("malformed");

    let jwk: JsonWebKey | null;
    try {
      jwk = await findJwk(parsed.kid);
    } catch {
      return rejectAccess("unknown_kid");
    }
    if (jwk === null) return rejectAccess("unknown_kid");

    let key: CryptoKey;
    try {
      key = await crypto.subtle.importKey(
        "jwk",
        jwk,
        { name: "RSASSA-PKCS1-v1_5", hash: "SHA-256" },
        false,
        ["verify"],
      );
    } catch {
      return rejectAccess("bad_signature");
    }

    const message = new TextEncoder().encode(`${parsed.headerPart}.${parsed.payloadPart}`);
    let signatureValid = false;
    try {
      signatureValid = await crypto.subtle.verify(
        { name: "RSASSA-PKCS1-v1_5" },
        key,
        asArrayBuffer(parsed.signature),
        asArrayBuffer(message),
      );
    } catch {
      return rejectAccess("bad_signature");
    }
    if (!signatureValid) return rejectAccess("bad_signature");

    const claims = parseJsonObject(parsed.payload);
    if (claims === null) return rejectAccess("bad_claims");
    const claimError = validateClaims(claims, issuer, audience);
    if (claimError !== null) return rejectAccess(claimError);

    return {
      email: claims.email as string,
      subject: claims.sub as string,
    };
  };

  return { verify };
}

function rejectAccess(reason: RejectionReason): null {
  signal("cf-access.rejected", { reason, cooldownKey: reason });
  return null;
}

function parseAssertion(assertion: string): ParsedAssertion | null {
  const parts = assertion.split(".");
  if (parts.length !== 3 || parts.some((part) => part.length === 0)) return null;
  const [headerPart, payloadPart, signaturePart] = parts as [string, string, string];
  const headerBytes = decodeBase64Url(headerPart);
  const payload = decodeBase64Url(payloadPart);
  const signature = decodeBase64Url(signaturePart);
  if (headerBytes === null || payload === null || signature === null) return null;

  const header = parseJsonObject(headerBytes) as AccessJwtHeader | null;
  if (header === null || header.alg !== "RS256" || typeof header.kid !== "string" || header.kid.length === 0) {
    return null;
  }
  return { headerPart, payloadPart, signature, payload, kid: header.kid };
}

function parseJwks(body: unknown): Map<string, JsonWebKey> | null {
  if (body === null || typeof body !== "object" || !("keys" in body)) return null;
  const keys = body.keys;
  if (!Array.isArray(keys)) return null;

  const parsed = new Map<string, JsonWebKey>();
  for (const candidate of keys) {
    if (candidate === null || typeof candidate !== "object" || !("kid" in candidate)) continue;
    const kid = candidate.kid;
    if (typeof kid !== "string" || kid.length === 0) continue;
    parsed.set(kid, candidate as JsonWebKey);
  }
  return parsed;
}

function decodeBase64Url(segment: string): Uint8Array | null {
  if (!BASE64URL_RE.test(segment) || segment.length % 4 === 1) return null;
  try {
    return b64urlDecode(segment);
  } catch {
    return null;
  }
}

function parseJsonObject(bytes: Uint8Array): Record<string, unknown> | null {
  try {
    const text = new TextDecoder("utf-8", { fatal: true }).decode(bytes);
    const value: unknown = JSON.parse(text);
    if (value === null || typeof value !== "object" || Array.isArray(value)) return null;
    return value as Record<string, unknown>;
  } catch {
    return null;
  }
}

function validateClaims(
  claims: Record<string, unknown>,
  issuer: string,
  audience: string,
): RejectionReason | null {
  if (claims.iss !== issuer) return "bad_issuer";
  if (!Array.isArray(claims.aud) || !claims.aud.includes(audience)) return "bad_audience";

  const nowMs = Date.now();
  if (!isFiniteNumeric(claims.exp)) return "bad_claims";
  const expMs = (claims.exp as number) * 1000;
  if (!Number.isFinite(expMs)) return "bad_claims";
  if (expMs <= nowMs - ACCESS_CLOCK_SKEW_MS) return "expired";

  if (!isFiniteNumeric(claims.iat)) return "bad_claims";
  const issuedAtMs = (claims.iat as number) * 1000;
  if (!Number.isFinite(issuedAtMs) || issuedAtMs > nowMs + ACCESS_CLOCK_SKEW_MS) return "bad_claims";

  const notBefore = claims.nbf;
  if (notBefore !== undefined) {
    if (!isFiniteNumeric(notBefore)) return "bad_claims";
    const notBeforeMs = (notBefore as number) * 1000;
    if (!Number.isFinite(notBeforeMs) || notBeforeMs > nowMs + ACCESS_CLOCK_SKEW_MS) return "bad_claims";
  }

  if (
    typeof claims.email !== "string" ||
    claims.email.length === 0 ||
    new TextEncoder().encode(claims.email).byteLength > MAX_ACCESS_EMAIL_UTF8_BYTES
  ) {
    return "bad_claims";
  }
  if (typeof claims.sub !== "string" || claims.sub.length === 0) return "bad_claims";
  return null;
}

function isFiniteNumeric(value: unknown): value is number {
  return typeof value === "number" && Number.isFinite(value);
}

function asArrayBuffer(bytes: Uint8Array): ArrayBuffer {
  return bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength) as ArrayBuffer;
}
