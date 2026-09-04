/**
 * Verifies Turnstile tokens through a durable one-time proof reservation.
 * Signup protocols call this service before starting abuse-sensitive OAuth or email work.
 * Hash-based idempotency prevents duplicate provider checks and token replay across restarts.
 */

import { createHash, randomUUID } from "node:crypto";
import { GATEWAY_OUTBOUND_ENDPOINTS } from "./gateway-config.ts";
import { fetchBoundedJson, ProviderRequestError, type GatewayFetch } from "./provider-http.ts";
import { TURNSTILE_PROOF_TTL_MS, type GatewayStateStore } from "./state-store.ts";

const TURNSTILE_TOKEN_MAX_BYTES = 2_048;
const TURNSTILE_TIMEOUT_MS = 5_000;
const TURNSTILE_RESPONSE_MAX_BYTES = 32 * 1024;
const EXPECTED_HOSTNAME = "dashboard.roosttt.com";
const EXPECTED_ACTION = "signup";

type VerificationOutcome = "verified" | "rejected" | "retryable";

export interface TurnstileVerifierOptions {
  store: GatewayStateStore;
  secret: string;
  fetch?: GatewayFetch;
  now?: () => number;
  createId?: () => string;
  /** Deterministic local fixture seam. Production construction must omit it. */
  endpoint?: string;
}

function validSuccess(value: unknown, nowMs: number): boolean {
  if (value === null || typeof value !== "object" || Array.isArray(value)) return false;
  const result = value as Record<string, unknown>;
  if (
    result.success !== true
    || result.hostname !== EXPECTED_HOSTNAME
    || result.action !== EXPECTED_ACTION
    || typeof result.challenge_ts !== "string"
  ) return false;
  const challengeAtMs = Date.parse(result.challenge_ts);
  return Number.isFinite(challengeAtMs)
    && challengeAtMs <= nowMs + 30_000
    && nowMs - challengeAtMs <= TURNSTILE_PROOF_TTL_MS;
}

export class TurnstileVerifier {
  private readonly fetchImpl: GatewayFetch;
  private readonly now: () => number;
  private readonly createId: () => string;
  private readonly endpoint: string;

  constructor(private readonly options: TurnstileVerifierOptions) {
    if (!options.secret || Buffer.byteLength(options.secret, "utf8") > 64 * 1024) {
      throw new Error("Turnstile secret is invalid");
    }
    this.fetchImpl = options.fetch ?? globalThis.fetch;
    this.now = options.now ?? Date.now;
    this.createId = options.createId ?? randomUUID;
    this.endpoint = options.endpoint ?? GATEWAY_OUTBOUND_ENDPOINTS.turnstileSiteverify;
  }

  private async request(
    token: string,
    clientIp: string,
    idempotencyKey: string,
  ): Promise<VerificationOutcome> {
    const body = new URLSearchParams({
      secret: this.options.secret,
      response: token,
      remoteip: clientIp,
      idempotency_key: idempotencyKey,
    });
    try {
      const { response, value } = await fetchBoundedJson(this.fetchImpl, this.endpoint, {
        method: "POST",
        headers: { "content-type": "application/x-www-form-urlencoded" },
        body,
      }, {
        timeoutMs: TURNSTILE_TIMEOUT_MS,
        maxResponseBytes: TURNSTILE_RESPONSE_MAX_BYTES,
      });
      if (response.status === 408 || response.status === 429 || response.status >= 500) return "retryable";
      if (!response.ok) return "rejected";
      return validSuccess(value, this.now()) ? "verified" : "rejected";
    } catch (error) {
      if (error instanceof ProviderRequestError) return "retryable";
      return "retryable";
    }
  }

  async verify(tokenRaw: unknown, clientIp: string): Promise<boolean> {
    if (
      typeof tokenRaw !== "string"
      || tokenRaw.length === 0
      || Buffer.byteLength(tokenRaw, "utf8") > TURNSTILE_TOKEN_MAX_BYTES
      || /[\u0000-\u001f\u007f]/u.test(tokenRaw)
    ) return false;
    const tokenHash = createHash("sha256").update(tokenRaw, "utf8").digest("hex");
    const reservation = this.options.store.beginTurnstileVerification(
      tokenHash,
      this.createId(),
      this.now(),
    );
    if (!reservation.mayVerify) return false;
    let outcome = await this.request(tokenRaw, clientIp, reservation.idempotencyKey);
    if (outcome === "retryable") {
      outcome = await this.request(tokenRaw, clientIp, reservation.idempotencyKey);
    }
    if (outcome === "verified") {
      return this.options.store.markTurnstileVerified(tokenHash, reservation.idempotencyKey, this.now());
    }
    if (outcome === "rejected") {
      this.options.store.markTurnstileFailed(tokenHash, reservation.idempotencyKey);
    }
    return false;
  }
}
