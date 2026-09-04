// Owns validation and canonical naming rules for durable registry state.
// Registry schema, row mappers, and domain stores share these checks.
// Central checks keep corrupt data distinct from invalid caller input.
import { randomBytes } from "node:crypto";
import { join, resolve } from "node:path";
import { normalizeAccountEmail } from "@roost/shared/native-credentials";
import {
  ACCOUNT_STATES,
  COORDINATOR_STATES,
  GOOGLE_IDENTITY_ISSUER,
  PROVISIONING_ASSERTION_PURPOSES,
  PROVISIONING_JOB_KINDS,
  type AccountState,
  type CoordinatorState,
  type ProvisioningAssertionInput,
  type ProvisioningAssertionPurpose,
  type ProvisioningJobKind,
} from "./registry-model.ts";

const UUID_RE = /^[0-9a-f]{8}-[0-9a-f]{4}-[1-5][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;
const IMAGE_DIGEST_RE = /^sha256:[0-9a-f]{64}$/;
export const ROUTE_KEY_RE = /^[0-9a-f]{64}$/;
export const SAFE_LEASE_VALUE_RE = /^[A-Za-z0-9][A-Za-z0-9._:-]{0,127}$/;
export const MAX_ERROR_BYTES = 2_048;
const SHA256_HEX_RE = /^[0-9a-f]{64}$/;
const GOOGLE_SUBJECT_MAX_BYTES = 255;
export const MAX_PROVISIONING_CLAIM_LIMIT = 100;
export const MAX_LEASE_DURATION_MS = 24 * 60 * 60 * 1000;
export const MAX_LINK_TICKET_LIFETIME_MS = 5 * 60 * 1000;
export const DEFAULT_ROOT = "/srv/data/roost";

export class SaasRegistryError extends Error {
  constructor(
    message: string,
    readonly code: "invalid" | "conflict" | "not-found" | "corrupt" | "lease-held",
  ) {
    super(message);
    this.name = "SaasRegistryError";
  }
}

export function checkedNow(now: () => number): number {
  const value = now();
  if (!Number.isSafeInteger(value) || value < 0) {
    throw new SaasRegistryError("registry clock returned an invalid timestamp", "invalid");
  }
  return value;
}

export function assertCanonicalUuid(value: string, name = "UUID"): string {
  if (!UUID_RE.test(value)) throw new SaasRegistryError(`${name} must be a canonical UUID`, "invalid");
  return value;
}

export function assertImmutableImageDigest(value: string): string {
  if (!IMAGE_DIGEST_RE.test(value)) {
    throw new SaasRegistryError("coordinator image must be an immutable sha256 digest", "invalid");
  }
  return value;
}

export function createTenantRouteKey(): string {
  return randomBytes(32).toString("hex");
}

export function assertTenantRouteKey(value: string): string {
  if (typeof value !== "string" || !ROUTE_KEY_RE.test(value)) {
    throw new SaasRegistryError("tenant route key must be exactly 64 lowercase hex characters", "invalid");
  }
  return value;
}

export function assertCanonicalGoogleIssuer(value: string): typeof GOOGLE_IDENTITY_ISSUER {
  if (value !== GOOGLE_IDENTITY_ISSUER) {
    throw new SaasRegistryError("federated identity issuer must be canonical Google issuer", "invalid");
  }
  return GOOGLE_IDENTITY_ISSUER;
}

export function assertGoogleIdentitySubject(value: string): string {
  if (
    typeof value !== "string"
    || value.length === 0
    || Buffer.byteLength(value, "utf8") > GOOGLE_SUBJECT_MAX_BYTES
    || /[\u0000-\u001f\u007f-\u009f]/u.test(value)
  ) {
    throw new SaasRegistryError("Google identity subject is invalid", "invalid");
  }
  return value;
}

export function assertSha256Hex(value: string, name = "SHA-256 hash"): string {
  if (typeof value !== "string" || !SHA256_HEX_RE.test(value)) {
    throw new SaasRegistryError(`${name} must be exactly 64 lowercase hex characters`, "invalid");
  }
  return value;
}

export function assertNormalizedEmail(value: string): string {
  const normalized = normalizeAccountEmail(value);
  if (!normalized || normalized !== value) {
    throw new SaasRegistryError("email must be normalized", "invalid");
  }
  return value;
}

export function assertSafeTimestamp(value: number, field: string): number {
  if (!Number.isSafeInteger(value) || value < 0) {
    throw new SaasRegistryError(`${field} must be a nonnegative safe integer`, "invalid");
  }
  return value;
}

export function assertRedactedError(value: string): string {
  if (
    typeof value !== "string"
    || value.length === 0
    || /[\u0000-\u001f\u007f-\u009f]/u.test(value)
    || Buffer.byteLength(value, "utf8") > MAX_ERROR_BYTES
  ) {
    throw new SaasRegistryError("invalid redacted provisioning error", "invalid");
  }
  return value;
}

export function assertProvisioningKind(value: string): ProvisioningJobKind {
  if (!(PROVISIONING_JOB_KINDS as readonly string[]).includes(value)) {
    throw new SaasRegistryError("invalid provisioning job kind", "invalid");
  }
  return value as ProvisioningJobKind;
}

export function assertAssertionInput(
  input: ProvisioningAssertionInput,
  kind: ProvisioningJobKind,
  expectedRouteKey?: string,
): ProvisioningAssertionInput {
  if (!(PROVISIONING_ASSERTION_PURPOSES as readonly string[]).includes(input.purpose)) {
    throw new SaasRegistryError("invalid provisioning assertion purpose", "invalid");
  }
  const expectedPurpose: ProvisioningAssertionPurpose = kind === "google-link" ? "link" : "continue";
  if (kind === "verified-email" || input.purpose !== expectedPurpose) {
    throw new SaasRegistryError("provisioning assertion purpose does not match job kind", "invalid");
  }
  const routeKey = assertTenantRouteKey(input.routeKey);
  if (expectedRouteKey !== undefined && routeKey !== expectedRouteKey) {
    throw new SaasRegistryError("provisioning assertion route does not match account", "conflict");
  }
  const deviceFingerprint = assertSha256Hex(input.deviceFingerprint, "device fingerprint");
  const jti = assertCanonicalUuid(input.jti, "assertion jti");
  const issuedAtMs = assertSafeTimestamp(input.issuedAtMs, "assertion issuedAtMs");
  const expiresAtMs = assertSafeTimestamp(input.expiresAtMs, "assertion expiresAtMs");
  if (expiresAtMs <= issuedAtMs || expiresAtMs - issuedAtMs > MAX_LINK_TICKET_LIFETIME_MS) {
    throw new SaasRegistryError("invalid provisioning assertion lifetime", "invalid");
  }
  return { purpose: input.purpose, routeKey, deviceFingerprint, jti, issuedAtMs, expiresAtMs };
}

export function corrupt(message: string): never {
  throw new SaasRegistryError(message, "corrupt");
}

function uuidHex(id: string): string {
  return assertCanonicalUuid(id).replaceAll("-", "");
}

export function coordinatorHostname(id: string): string {
  return `c-${uuidHex(id)}.dashboard.roosttt.com`;
}

export function coordinatorContainerName(id: string): string {
  return `roost-coord-${uuidHex(id)}`;
}

export function coordinatorDataDir(rootDir: string, id: string): string {
  return join(resolve(rootDir), "instances", assertCanonicalUuid(id), "data");
}

export function isAccountState(value: string): value is AccountState {
  return (ACCOUNT_STATES as readonly string[]).includes(value);
}

export function isCoordinatorState(value: string): value is CoordinatorState {
  return (COORDINATOR_STATES as readonly string[]).includes(value);
}

export function checkedTimestamp(value: number | null, field: string): number | null {
  if (value === null) return null;
  if (!Number.isSafeInteger(value) || value < 0) {
    throw new SaasRegistryError(`registry row has invalid ${field}`, "corrupt");
  }
  return value;
}
