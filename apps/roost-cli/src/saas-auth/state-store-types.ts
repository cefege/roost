/**
 * Defines the durable signup gateway states shared by protocol and persistence code.
 * The public state-store entry point re-exports these contracts for existing callers.
 * Keeping wire-facing shapes here lets storage operations split without changing the API.
 */

export const EMAIL_CHALLENGE_TTL_MS = 7 * 24 * 60 * 60 * 1_000;
export const OAUTH_ATTEMPT_TTL_MS = 10 * 60 * 1_000;
export const MAX_DURABLE_RATE_BUCKETS = 10_000;
export const TURNSTILE_PROOF_TTL_MS = 300_000;
export const MAX_DURABLE_TURNSTILE_PROOFS = 10_000;

export const GATEWAY_RATE_SCOPES = [
  "email-start-ip",
  "email-start-email",
  "google-signup-ip",
  "google-login-ip",
] as const;

export type GatewayRateScope = typeof GATEWAY_RATE_SCOPES[number];
export type OAuthIntent = "login" | "signup" | "link";
export type GatewayResultState =
  | "pending"
  | "awaiting-device"
  | "ready"
  | "proof-required"
  | "capacity"
  | "failed";

export type GatewayStateErrorCode = "invalid" | "conflict" | "capacity" | "corrupt";

export class GatewayStateError extends Error {
  constructor(message: string, readonly code: GatewayStateErrorCode) {
    super(message);
  }
}

export interface OpenGatewayStateStoreOptions {
  path?: string;
  oauthStateKey: string;
  now?: () => number;
  createId?: () => string;
}

export interface RateBucketResult {
  allowed: boolean;
  remaining: number;
  retryAtMs: number;
}

export interface ConsumeRateBucketOptions {
  scope: GatewayRateScope;
  key: string;
  limit: number;
  windowMs: number;
  nowMs?: number;
}

export interface TurnstileVerification {
  idempotencyKey: string;
  mayVerify: boolean;
}

export interface GatewayEmailChallenge {
  id: string;
  tokenHash: string;
  emailNormalized: string;
  state: "pending" | "verified" | "consumed";
  createdAtMs: number;
  expiresAtMs: number;
  verifiedAtMs: number | null;
  consumedAtMs: number | null;
  outboxId: string;
}

export interface CreateEmailChallengeOptions {
  id?: string;
  outboxId?: string;
  tokenHash: string;
  emailNormalized: string;
  encryptedPayload: string;
  nowMs?: number;
}

export interface SignupEmailLease {
  id: string;
  challengeId: string;
  recipient: string;
  encryptedPayload: string;
  attempt: number;
  leaseToken: string;
}

export interface ClaimSignupEmailOptions {
  nowMs?: number;
  leaseDurationMs: number;
  limit: number;
}

export interface OAuthAttemptInput {
  browserCookie: string;
  oauthCookie: string;
  state: string;
  pkceVerifier: string;
  nonce: string;
  intent: OAuthIntent;
  routeKey?: string;
  linkTicket?: string;
  proofAtMs?: number;
  nowMs?: number;
}

export interface ConsumedOAuthAttempt {
  id: string;
  intent: OAuthIntent;
  pkceVerifier: string;
  nonce: string;
  routeKey: string | null;
  linkTicket: string | null;
  proofAtMs: number | null;
  createdAtMs: number;
  expiresAtMs: number;
  browserCookieHash: string;
}

export interface CreateResultReceiptOptions {
  receipt: string;
  browserCookie: string;
  jobId: string;
  nowMs?: number;
  expiresAtMs: number;
}

export interface CreateHashedResultReceiptOptions {
  receipt: string;
  browserCookieHash: string;
  jobId: string;
  nowMs?: number;
  expiresAtMs: number;
}

export interface GatewayResultReceipt {
  jobId: string;
  state: GatewayResultState;
  routeKey: string | null;
  assertion: string | null;
  boundFingerprint: string | null;
  createdAtMs: number;
  expiresAtMs: number;
}

export interface SetResultOutcomeOptions {
  jobId: string;
  state: Exclude<GatewayResultState, "pending" | "ready">;
  routeKey?: string;
  assertionInput?: string;
  nowMs?: number;
}
