// Defines provisioning submissions, statuses, limits, and canonical checks.
// The submission worker and durable job loop share these side-effect-free rules.
// Proof freshness and idempotency remain identical across every entry point.
import { createHash } from "node:crypto";
import { normalizeAccountEmail } from "@roost/shared/native-credentials";
import {
  type SaasRegistry,
  assertCanonicalUuid,
  assertSha256Hex,
  type AccountReservation,
} from "./registry.ts";
import { GOOGLE_IDENTITY_ISSUER } from "./registry-model.ts";
import type { ProvisioningAdmission, SaasLifecycle } from "./lifecycle.ts";

export const DEFAULT_JOB_LEASE_MS = 5 * 60_000;
export const DEFAULT_RETRY_DELAY_MS = 5_000;
export const MAX_RETRY_DELAY_MS = 60_000;
export const MAX_JOB_ATTEMPTS = 8;
export const MAX_GOOGLE_PROOF_LIFETIME_MS = 10 * 60_000;
export const MAX_EMAIL_PROOF_LIFETIME_MS = 7 * 24 * 60 * 60_000;
export const WORKER_LEASE_RESOURCE = "provisioning-worker";

type SafeTerminalState = "proof-required" | "capacity" | "failed";
export type ProvisioningPublicState =
  | "pending"
  | "awaiting-device"
  | "ready"
  | SafeTerminalState;

export interface VerifiedEmailSubmission {
  kind: "verified-email";
  challengeId: string;
  emailNormalized: string;
  activationTokenHash: string;
  verifiedAtMs: number;
  expiresAtMs: number;
  idempotencyKey: string;
}

export interface GoogleSubmission {
  kind: "google-signup" | "google-login";
  issuer: string;
  subject: string;
  emailNormalized: string;
  verifiedAtMs: number;
  expiresAtMs: number;
  idempotencyKey: string;
}

export interface VerifiedLinkTicket {
  ticketJti: string;
  accountId: string;
  coordinatorId: string;
  routeKey: string;
  deviceFingerprint: string;
  issuedAtMs: number;
  expiresAtMs: number;
}

export interface GoogleLinkSubmission {
  kind: "google-link";
  issuer: string;
  subject: string;
  emailNormalized: string;
  verifiedAtMs: number;
  expiresAtMs: number;
  idempotencyKey: string;
  ticket: VerifiedLinkTicket;
}

export type ProvisioningSubmission =
  | VerifiedEmailSubmission
  | GoogleSubmission
  | GoogleLinkSubmission;

export type ProvisioningSubmitResult =
  | { state: "pending"; jobId: string; retryAfterMs: number }
  | { state: SafeTerminalState };

export interface CanonicalAssertionInputs {
  purpose: "continue" | "link";
  accountId: string;
  coordinatorId: string;
  routeKey: string;
  identityIssuer: typeof GOOGLE_IDENTITY_ISSUER;
  identitySubject: string;
  emailNormalized: string;
  deviceFingerprint?: string;
  jti?: string;
  issuedAtMs?: number;
  expiresAtMs?: number;
}

export type ProvisioningStatus =
  | { state: "pending"; retryAfterMs: number }
  | { state: "ready"; routeKey: string }
  | {
    state: "awaiting-device" | "ready";
    routeKey: string;
    assertionInputs: CanonicalAssertionInputs;
  }
  | { state: "failed" };

export interface ProvisioningWorkerOptions {
  registry: SaasRegistry;
  lifecycle: SaasLifecycle;
  admission: ProvisioningAdmission;
  imageDigest: string;
  now?: () => number;
  leaseDurationMs?: number;
  retryDelayMs?: number;
}

function sha256Hex(value: string): string {
  return createHash("sha256").update(value, "utf8").digest("hex");
}

export function checkedTimestamp(value: number, name: string): number {
  if (!Number.isSafeInteger(value) || value < 0) throw new Error(`${name} is invalid`);
  return value;
}

export function checkedEmail(value: string): string {
  const normalized = normalizeAccountEmail(value);
  if (!normalized || normalized !== value) throw new Error("verified email must already be normalized");
  return normalized;
}

export function idempotencyHash(value: string): string {
  return sha256Hex(assertSha256Hex(value, "idempotency key"));
}

export function assertFreshProof(
  now: number,
  verifiedAtMsRaw: number,
  expiresAtMsRaw: number,
  maximumLifetimeMs: number,
): { verifiedAtMs: number; expiresAtMs: number } {
  const verifiedAtMs = checkedTimestamp(verifiedAtMsRaw, "verifiedAtMs");
  const expiresAtMs = checkedTimestamp(expiresAtMsRaw, "expiresAtMs");
  if (verifiedAtMs > now || now >= expiresAtMs || expiresAtMs - verifiedAtMs > maximumLifetimeMs) {
    throw new Error("verified identity proof is stale");
  }
  return { verifiedAtMs, expiresAtMs };
}

export function ordinalOneReservation(
  registry: SaasRegistry,
  accountId: string,
  coordinatorId: string,
  resumed: boolean,
): AccountReservation {
  const account = registry.getAccount(accountId);
  const coordinator = registry.getCoordinator(coordinatorId);
  if (coordinator.accountId !== account.id || coordinator.ordinal !== 1 || coordinator.routeKey !== account.routeKey) {
    throw new Error("provisioning account topology is inconsistent");
  }
  return { account, coordinator, resumed };
}

export function safeError(error: unknown): string {
  const raw = error instanceof Error ? error.message : String(error);
  return raw
    .replace(/(?:Bearer\s+|token=)[^\s&#]+/gi, "[credential]")
    .replace(/[A-Z0-9._%+-]+@[A-Z0-9.-]+\.[A-Z]{2,}/gi, "[email]")
    .replace(/[A-Za-z0-9_-]{40,}/g, "[secret]")
    .replace(/[\u0000-\u001f\u007f-\u009f]/gu, " ")
    .slice(0, 1_024) || "provisioning failed";
}
