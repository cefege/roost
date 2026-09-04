/**
 * Queues encrypted signup emails and drives their leased retryable delivery lifecycle.
 * Email signup writes challenges through this outbox, while the gateway runs its delivery loop.
 * Durable leases and idempotency keys prevent duplicate sends after crashes or concurrent claims.
 */

import { createHash, randomBytes, randomUUID } from "node:crypto";
import {
  createEmailOutboxPayloadCipher,
  EmailOutboxPayloadError,
  type EmailOutboxPayload,
  type EmailOutboxPayloadCipher,
} from "@roost/shared/email-payload";
import type { EmailClient, ResendFailureReason } from "@roost/shared/email-client";
import { backoffDelayMs } from "@roost/shared/retry";
import {
  EMAIL_CHALLENGE_TTL_MS,
  type GatewayEmailChallenge,
  type GatewayStateStore,
} from "./state-store.ts";

export interface QueueSignupVerificationOptions {
  emailNormalized: string;
  payload: EmailOutboxPayload | ((token: string) => EmailOutboxPayload);
  nowMs?: number;
}

export interface QueuedSignupVerification {
  challenge: GatewayEmailChallenge;
  /** Returned once for the verification URL; the store persists only its hash. */
  token: string;
}

export interface SignupEmailOutboxOptions {
  store: GatewayStateStore;
  emailOutboxKey: string;
  client: EmailClient;
  now?: () => number;
  leaseDurationMs?: number;
  batchSize?: number;
  maxAttempts?: number;
}

export interface SignupEmailRunResult {
  claimed: number;
  sent: number;
  retried: number;
  failed: number;
  leaseLost: number;
}

const OUTBOX_KIND = "signup-verification";
const DEFAULT_LEASE_MS = 60_000;
const DEFAULT_BATCH_SIZE = 10;
const DEFAULT_MAX_ATTEMPTS = 5;

function positive(value: number, name: string, max: number): number {
  if (!Number.isSafeInteger(value) || value <= 0 || value > max) throw new Error(`invalid signup email ${name}`);
  return value;
}

function safeReason(reason: ResendFailureReason | "payload_invalid" | "retry_exhausted"): string {
  return reason.length <= 128 ? reason : "network";
}

export class SignupEmailOutbox {
  private readonly store: GatewayStateStore;
  private readonly cipher: EmailOutboxPayloadCipher;
  private readonly client: EmailClient;
  private readonly now: () => number;
  private readonly leaseDurationMs: number;
  private readonly batchSize: number;
  private readonly maxAttempts: number;

  constructor(options: SignupEmailOutboxOptions) {
    this.store = options.store;
    this.cipher = createEmailOutboxPayloadCipher(options.emailOutboxKey);
    this.client = options.client;
    this.now = options.now ?? Date.now;
    this.leaseDurationMs = positive(options.leaseDurationMs ?? DEFAULT_LEASE_MS, "lease duration", 24 * 60 * 60 * 1_000);
    this.batchSize = positive(options.batchSize ?? DEFAULT_BATCH_SIZE, "batch size", 100);
    this.maxAttempts = positive(options.maxAttempts ?? DEFAULT_MAX_ATTEMPTS, "attempt limit", 1_000);
  }

  queueVerification(options: QueueSignupVerificationOptions): QueuedSignupVerification {
    const nowMs = options.nowMs ?? this.now();
    if (!Number.isSafeInteger(nowMs) || nowMs < 0) throw new Error("invalid signup email time");
    const challengeId = randomUUID();
    const outboxId = randomUUID();
    const token = randomBytes(32).toString("base64url");
    const tokenHash = createHash("sha256").update(token, "utf8").digest("hex");
    const payload = typeof options.payload === "function" ? options.payload(token) : options.payload;
    const encryptedPayload = this.cipher.encrypt({ outboxId, kind: OUTBOX_KIND }, payload);
    const challenge = this.store.createEmailChallenge({
      id: challengeId,
      outboxId,
      tokenHash,
      emailNormalized: options.emailNormalized,
      encryptedPayload,
      nowMs,
    });
    if (challenge.expiresAtMs !== nowMs + EMAIL_CHALLENGE_TTL_MS) throw new Error("signup email challenge lifetime mismatch");
    return { challenge, token };
  }

  async runOnce(): Promise<SignupEmailRunResult> {
    const result: SignupEmailRunResult = { claimed: 0, sent: 0, retried: 0, failed: 0, leaseLost: 0 };
    const leases = this.store.claimDueSignupEmails({
      nowMs: this.now(),
      leaseDurationMs: this.leaseDurationMs,
      limit: this.batchSize,
    });
    result.claimed = leases.length;
    for (const lease of leases) {
      if (lease.attempt > this.maxAttempts) {
        if (this.store.failSignupEmail(lease, safeReason("retry_exhausted"), this.now())) result.failed++;
        else result.leaseLost++;
        continue;
      }
      let payload: EmailOutboxPayload;
      try {
        payload = this.cipher.decrypt({ outboxId: lease.id, kind: OUTBOX_KIND }, lease.encryptedPayload);
      } catch (error) {
        if (!(error instanceof EmailOutboxPayloadError)) throw error;
        if (this.store.failSignupEmail(lease, safeReason("payload_invalid"), this.now())) result.failed++;
        else result.leaseLost++;
        continue;
      }
      const delivery = await this.client.send({
        recipient: lease.recipient,
        subject: payload.subject,
        html: payload.html,
        ...(payload.text === undefined ? {} : { text: payload.text }),
        idempotencyKey: lease.id,
      });
      if (delivery.outcome === "sent") {
        if (this.store.markSignupEmailSent(lease, delivery.providerMessageId, this.now())) result.sent++;
        else result.leaseLost++;
      } else if (delivery.outcome === "permanent") {
        if (this.store.failSignupEmail(lease, safeReason(delivery.reason), this.now())) result.failed++;
        else result.leaseLost++;
      } else {
        const backoffMs = backoffDelayMs(Math.max(0, lease.attempt - 1), { baseMs: 1_000, maxMs: 5 * 60_000 });
        const retryAfterMs = delivery.retryAfterMs === undefined || !Number.isFinite(delivery.retryAfterMs)
          ? 0
          : Math.min(60_000, Math.max(0, Math.floor(delivery.retryAfterMs)));
        if (this.store.rescheduleSignupEmail(lease, this.now() + Math.max(backoffMs, retryAfterMs), safeReason(delivery.reason))) result.retried++;
        else result.leaseLost++;
      }
    }
    return result;
  }
}
