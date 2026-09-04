// Federated assertions cross the dashboard-to-coordinator trust boundary, so
// their envelope, canonical claims, and key source are bounded before signature
// verification can authorize an identity or account-link operation.

import { constants, closeSync, fstatSync, openSync, readFileSync } from "node:fs";
import { normalizeAccountEmail } from "@roost/shared/native-credentials";
import { parseSshEd25519Line } from "../authorized-keys.ts";

export const FEDERATED_ASSERTION_ISSUER = "https://dashboard.roosttt.com/__roost/auth";
export const FEDERATED_ASSERTION_AUDIENCE = "roost-federated-identity";
export const GOOGLE_IDENTITY_ISSUER = "https://accounts.google.com";
export const IDENTITY_LINK_TICKET_AUDIENCE = "roost-saas-identity-link";

const ROUTE_OR_FINGERPRINT = /^[0-9a-f]{64}$/;
const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[1-5][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;
const BASE64URL = /^[A-Za-z0-9_-]+$/;
const MAX_ASSERTION_BYTES = 8_192;
const MAX_ID_BYTES = 255;
const MAX_CLOCK_SKEW_SECONDS = 30;
const SPKI_ED25519_PREFIX = new Uint8Array([
  0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
]);

export type FederatedAssertionPurpose = "continue" | "link";

export interface FederatedAssertionClaims {
  iss: typeof FEDERATED_ASSERTION_ISSUER;
  aud: typeof FEDERATED_ASSERTION_AUDIENCE;
  purpose: FederatedAssertionPurpose;
  account_id: string;
  coordinator_id: string;
  route_key: string;
  identity_issuer: typeof GOOGLE_IDENTITY_ISSUER;
  identity_subject: string;
  email_normalized: string;
  device_fp: string;
  jti: string;
  iat: number;
  exp: number;
}

function invalid(): never {
  throw new Error("invalid federated assertion");
}

function isPlainRecord(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function exactKeys(value: Record<string, unknown>, expected: readonly string[]): boolean {
  const actual = Object.keys(value).sort();
  const wanted = [...expected].sort();
  return actual.length === wanted.length && actual.every((key, index) => key === wanted[index]);
}

function boundedIdentifier(value: unknown): value is string {
  return typeof value === "string"
    && value.length > 0
    && Buffer.byteLength(value, "utf8") <= MAX_ID_BYTES
    && !/[\u0000-\u001f\u007f-\u009f]/u.test(value);
}

function decodeSegment(segment: string): Uint8Array {
  if (!BASE64URL.test(segment)) invalid();
  const decoded = new Uint8Array(Buffer.from(segment, "base64url"));
  if (Buffer.from(decoded).toString("base64url") !== segment) invalid();
  return decoded;
}

function arrayBuffer(bytes: Uint8Array): ArrayBuffer {
  return bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength) as ArrayBuffer;
}

function readVerificationKey(path: string): Uint8Array {
  let fd: number | undefined;
  try {
    fd = openSync(path, constants.O_RDONLY | constants.O_NOFOLLOW);
    const stat = fstatSync(fd);
    if (!stat.isFile() || stat.size <= 0 || stat.size > 1_024) invalid();
    const text = readFileSync(fd, "utf8");
    const lines = text.split(/\r?\n/u).filter((line) => line.trim().length > 0);
    if (lines.length !== 1) invalid();
    const parsed = parseSshEd25519Line(lines[0]!);
    if (!parsed || parsed.pubkey.byteLength !== 32) invalid();
    return new Uint8Array(parsed.pubkey);
  } catch {
    return invalid();
  } finally {
    if (fd !== undefined) closeSync(fd);
  }
}

async function importVerificationKey(raw: Uint8Array): Promise<CryptoKey> {
  const spki = new Uint8Array(SPKI_ED25519_PREFIX.byteLength + raw.byteLength);
  spki.set(SPKI_ED25519_PREFIX);
  spki.set(raw, SPKI_ED25519_PREFIX.byteLength);
  return crypto.subtle.importKey("spki", arrayBuffer(spki), { name: "Ed25519" }, false, ["verify"]);
}

function parseClaims(payload: unknown, expectedPurpose: FederatedAssertionPurpose, nowMs: number): FederatedAssertionClaims {
  if (!isPlainRecord(payload) || !exactKeys(payload, [
    "iss", "aud", "purpose", "account_id", "coordinator_id", "route_key",
    "identity_issuer", "identity_subject", "email_normalized", "device_fp",
    "jti", "iat", "exp",
  ])) invalid();
  if (
    payload.iss !== FEDERATED_ASSERTION_ISSUER
    || payload.aud !== FEDERATED_ASSERTION_AUDIENCE
    || payload.purpose !== expectedPurpose
    || payload.identity_issuer !== GOOGLE_IDENTITY_ISSUER
    || !boundedIdentifier(payload.account_id)
    || !boundedIdentifier(payload.coordinator_id)
    || !boundedIdentifier(payload.identity_subject)
    || typeof payload.route_key !== "string"
    || !ROUTE_OR_FINGERPRINT.test(payload.route_key)
    || typeof payload.device_fp !== "string"
    || !ROUTE_OR_FINGERPRINT.test(payload.device_fp)
    || typeof payload.jti !== "string"
    || !UUID.test(payload.jti)
  ) invalid();
  const email = typeof payload.email_normalized === "string"
    ? normalizeAccountEmail(payload.email_normalized)
    : null;
  if (!email || email !== payload.email_normalized) invalid();
  if (!Number.isSafeInteger(payload.iat) || !Number.isSafeInteger(payload.exp)) invalid();
  const iat = payload.iat as number;
  const exp = payload.exp as number;
  const now = Math.floor(nowMs / 1_000);
  if (
    iat < 0
    || exp <= iat
    || exp > iat + 300
    || exp <= now
    || iat > now + MAX_CLOCK_SKEW_SECONDS
  ) invalid();
  return payload as unknown as FederatedAssertionClaims;
}

export type VerifyFederatedAssertion = (
  assertion: string,
  purpose: FederatedAssertionPurpose,
  nowMs?: number,
) => Promise<FederatedAssertionClaims>;

/** Loads the immutable managed public key once, then verifies every compact
 * assertion locally. No provider token or network dependency enters a tenant. */
export function createFederatedAssertionVerifier(path: string): VerifyFederatedAssertion {
  const key = importVerificationKey(readVerificationKey(path));
  return async (assertion, purpose, nowMs = Date.now()) => {
    if (Buffer.byteLength(assertion, "utf8") > MAX_ASSERTION_BYTES) invalid();
    const parts = assertion.split(".");
    if (parts.length !== 3 || parts.some((part) => part.length === 0)) invalid();
    const [headerPart, payloadPart, signaturePart] = parts as [string, string, string];
    let header: unknown;
    try {
      header = JSON.parse(new TextDecoder().decode(decodeSegment(headerPart)));
    } catch {
      invalid();
    }
    if (
      !isPlainRecord(header)
      || !exactKeys(header, Object.hasOwn(header, "kid") ? ["alg", "typ", "kid"] : ["alg", "typ"])
      || header.alg !== "EdDSA"
      || header.typ !== "JWT"
      || (Object.hasOwn(header, "kid") && !boundedIdentifier(header.kid))
    ) invalid();
    const signature = decodeSegment(signaturePart);
    if (signature.byteLength !== 64) invalid();
    const message = new TextEncoder().encode(`${headerPart}.${payloadPart}`);
    const verified = await crypto.subtle.verify(
      { name: "Ed25519" },
      await key,
      arrayBuffer(signature),
      arrayBuffer(message),
    );
    if (!verified) invalid();
    let payload: unknown;
    try {
      payload = JSON.parse(new TextDecoder().decode(decodeSegment(payloadPart)));
    } catch {
      invalid();
    }
    return parseClaims(payload, purpose, nowMs);
  };
}
