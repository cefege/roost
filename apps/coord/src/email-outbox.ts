// Owns the durable email outbox dispatch state machine used by the coordinator.
// Startup injects storage, delivery, cipher, timing, and diagnostic dependencies.
// The dispatcher performs no request-path I/O and starts only after migrations.
// Every completion respects the storage seam's opaque lease-token ownership.

import { createKyselyEmailOutboxStore } from "./email-outbox-store.ts";
import { log } from "@roost/shared/log";
import { backoffDelayMs, type BackoffOptions } from "@roost/shared/retry";
import type {
  EmailClient,
  EmailClock,
  EmailTimer,
  ResendEmailResult,
  ResendFailureReason,
} from "@roost/shared/email-client";
import {
  EmailOutboxPayloadError,
  type EmailOutboxPayload,
  type EmailOutboxPayloadCipher,
} from "@roost/shared/email-payload";
export { createKyselyEmailOutboxStore };

export interface EmailOutboxLease {
  id: string;
  kind: string;
  recipient: string;
  encryptedPayload: string;
  attempt: number;
  /** Opaque durable ownership token. Never log it or expose it beyond storage. */
  leaseToken: string;
}

export interface EmailOutboxClaimOptions {
  nowMs: number;
  leaseDurationMs: number;
  limit: number;
}

export interface EmailOutboxReschedule {
  nowMs: number;
  nextAttemptMs: number;
  reason: ResendFailureReason | "retry_exhausted";
}

export interface EmailOutboxFailure {
  nowMs: number;
  reason: ResendFailureReason | "payload_invalid" | "retry_exhausted";
}

/**
 * Storage is an explicit seam: a claim must atomically move due rows to
 * `sending`, persist a fresh opaque lease token, and return only that owner's
 * rows. All terminal/retry mutations must CAS on both ID and lease token.
 */
export interface EmailOutboxStore {
  claimDue(options: EmailOutboxClaimOptions): Promise<EmailOutboxLease[]>;
  markSent(lease: EmailOutboxLease, providerMessageId: string, nowMs: number): Promise<boolean>;
  reschedule(lease: EmailOutboxLease, update: EmailOutboxReschedule): Promise<boolean>;
  markFailed(lease: EmailOutboxLease, update: EmailOutboxFailure): Promise<boolean>;
}

export type EmailOutboxDiagnosticEvent =
  | "email_sent"
  | "email_retry_scheduled"
  | "email_failed"
  | "email_lease_lost";

export interface EmailOutboxDiagnosticFields extends Record<string, unknown> {
  outbox_id: string;
  kind: string;
  attempt: number;
  reason?: ResendFailureReason | "payload_invalid" | "retry_exhausted";
  delay_ms?: number;
}

export interface EmailOutboxDiagnostics {
  info(event: EmailOutboxDiagnosticEvent, fields: EmailOutboxDiagnosticFields): void;
  warn(event: EmailOutboxDiagnosticEvent, fields: EmailOutboxDiagnosticFields): void;
}

const defaultDiagnostics: EmailOutboxDiagnostics = {
  info: (event, fields) => log.info("email-outbox", event, fields),
  warn: (event, fields) => log.warn("email-outbox", event, fields),
};

const systemClock: EmailClock = {
  now: Date.now,
  setTimeout: (callback, delayMs) => setTimeout(callback, delayMs),
  clearTimeout: (timer) => clearTimeout(timer),
};

export interface EmailOutboxDispatcherOptions {
  store: EmailOutboxStore;
  client: EmailClient;
  cipher: EmailOutboxPayloadCipher;
  clock?: EmailClock;
  diagnostics?: EmailOutboxDiagnostics;
  leaseDurationMs?: number;
  pollIntervalMs?: number;
  batchSize?: number;
  maxAttempts?: number;
  backoff?: BackoffOptions;
  /** Maximum server-provided Retry-After delay accepted for one delivery. */
  maxRetryAfterMs?: number;
}

export interface EmailOutboxRunResult {
  claimed: number;
  sent: number;
  retried: number;
  failed: number;
}

export interface EmailOutboxDispatcher {
  /** Starts timer-owned background delivery; call only after migrations complete. */
  start(): void;
  /** Stops future polls and waits for a currently running pass to finish. */
  stop(): Promise<void>;
  /** Executes one worker pass; never call from a request or Sync path. */
  runOnce(): Promise<EmailOutboxRunResult>;
}

const DEFAULT_LEASE_DURATION_MS = 60_000;
const DEFAULT_POLL_INTERVAL_MS = 1_000;
const DEFAULT_BATCH_SIZE = 10;
const DEFAULT_MAX_ATTEMPTS = 5;
const DEFAULT_MAX_RETRY_AFTER_MS = 60_000;
const DEFAULT_BACKOFF: BackoffOptions = {
  baseMs: 1_000,
  maxMs: 5 * 60_000,
};

function validPositiveInteger(value: number, name: string): number {
  if (!Number.isSafeInteger(value) || value <= 0) throw new Error(`invalid email outbox ${name}`);
  return value;
}

function nextRetryDelay(
  result: Extract<ResendEmailResult, { outcome: "retry" }>,
  attempt: number,
  backoff: BackoffOptions,
  maxRetryAfterMs: number,
): number {
  const backoffMs = backoffDelayMs(Math.max(0, attempt - 1), backoff);
  const suppliedRetryAfterMs = result.retryAfterMs;
  const retryAfter = suppliedRetryAfterMs === undefined || !Number.isFinite(suppliedRetryAfterMs)
    ? 0
    : Math.min(maxRetryAfterMs, Math.max(0, Math.floor(suppliedRetryAfterMs)));
  return Math.max(backoffMs, retryAfter);
}

function incrementResult(
  result: EmailOutboxRunResult,
  outcome: "sent" | "retried" | "failed",
): void {
  result[outcome]++;
}

function unrefTimer(timer: EmailTimer): void {
  const candidate: unknown = timer;
  if (
    candidate !== null
    && typeof candidate === "object"
    && "unref" in candidate
    && typeof candidate.unref === "function"
  ) candidate.unref();
}

/** Creates a stoppable, timer-owned background dispatcher for one coordinator. */
export function createEmailOutboxDispatcher(options: EmailOutboxDispatcherOptions): EmailOutboxDispatcher {
  const clock = options.clock ?? systemClock;
  const diagnostics = options.diagnostics ?? defaultDiagnostics;
  const leaseDurationMs = validPositiveInteger(
    options.leaseDurationMs ?? DEFAULT_LEASE_DURATION_MS,
    "lease duration",
  );
  const pollIntervalMs = validPositiveInteger(
    options.pollIntervalMs ?? DEFAULT_POLL_INTERVAL_MS,
    "poll interval",
  );
  const batchSize = validPositiveInteger(options.batchSize ?? DEFAULT_BATCH_SIZE, "batch size");
  const maxAttempts = validPositiveInteger(options.maxAttempts ?? DEFAULT_MAX_ATTEMPTS, "max attempts");
  const maxRetryAfterMs = validPositiveInteger(
    options.maxRetryAfterMs ?? DEFAULT_MAX_RETRY_AFTER_MS,
    "maximum Retry-After",
  );
  const backoff = options.backoff ?? DEFAULT_BACKOFF;
  if (
    !Number.isFinite(backoff.baseMs)
    || !Number.isFinite(backoff.maxMs)
    || backoff.baseMs <= 0
    || backoff.maxMs <= 0
  ) throw new Error("invalid email outbox backoff");

  let timer: EmailTimer | null = null;
  let stopped = false;
  let started = false;
  let activeRun: Promise<EmailOutboxRunResult> | null = null;

  const runOnce = async (): Promise<EmailOutboxRunResult> => {
    if (activeRun) return activeRun;

    const run = (async (): Promise<EmailOutboxRunResult> => {
      const outcome: EmailOutboxRunResult = { claimed: 0, sent: 0, retried: 0, failed: 0 };
      if (stopped) return outcome;

      while (!stopped) {
        const leases = await options.store.claimDue({
          nowMs: clock.now(),
          leaseDurationMs,
          limit: batchSize,
        });
        outcome.claimed += leases.length;
        if (leases.length === 0) break;

        for (const lease of leases) {
          if (stopped) break;
          if (lease.attempt > maxAttempts) {
            const applied = await options.store.markFailed(lease, {
              nowMs: clock.now(),
              reason: "retry_exhausted",
            });
            if (applied) {
              incrementResult(outcome, "failed");
              diagnostics.warn("email_failed", {
                outbox_id: lease.id,
                kind: lease.kind,
                attempt: lease.attempt,
                reason: "retry_exhausted",
              });
            } else {
              diagnostics.warn("email_lease_lost", {
                outbox_id: lease.id,
                kind: lease.kind,
                attempt: lease.attempt,
              });
            }
            continue;
          }
          let payload: EmailOutboxPayload;
          try {
            payload = options.cipher.decrypt({ outboxId: lease.id, kind: lease.kind }, lease.encryptedPayload);
          } catch (error) {
            if (!(error instanceof EmailOutboxPayloadError)) throw error;
            const applied = await options.store.markFailed(lease, {
              nowMs: clock.now(),
              reason: "payload_invalid",
            });
            if (applied) {
              incrementResult(outcome, "failed");
              diagnostics.warn("email_failed", {
                outbox_id: lease.id,
                kind: lease.kind,
                attempt: lease.attempt,
                reason: "payload_invalid",
              });
            } else {
              diagnostics.warn("email_lease_lost", {
                outbox_id: lease.id,
                kind: lease.kind,
                attempt: lease.attempt,
              });
            }
            continue;
          }

          let delivery: ResendEmailResult;
          try {
            delivery = await options.client.send({
              recipient: lease.recipient,
              subject: payload.subject,
              html: payload.html,
              ...(payload.text === undefined ? {} : { text: payload.text }),
              // The persisted ID, not the idempotency_key column or a retry UUID,
              // is the documented Resend Idempotency-Key for every attempt.
              idempotencyKey: lease.id,
            });
          } catch {
            delivery = { outcome: "retry", reason: "network" };
          }
          const nowMs = clock.now();

          if (delivery.outcome === "sent") {
            const applied = await options.store.markSent(lease, delivery.providerMessageId, nowMs);
            if (applied) {
              incrementResult(outcome, "sent");
              diagnostics.info("email_sent", {
                outbox_id: lease.id,
                kind: lease.kind,
                attempt: lease.attempt,
              });
            } else {
              diagnostics.warn("email_lease_lost", {
                outbox_id: lease.id,
                kind: lease.kind,
                attempt: lease.attempt,
              });
            }
            continue;
          }

          if (delivery.outcome === "permanent" || lease.attempt >= maxAttempts) {
            const reason = delivery.outcome === "permanent" ? delivery.reason : "retry_exhausted";
            const applied = await options.store.markFailed(lease, { nowMs, reason });
            if (applied) {
              incrementResult(outcome, "failed");
              diagnostics.warn("email_failed", {
                outbox_id: lease.id,
                kind: lease.kind,
                attempt: lease.attempt,
                reason,
              });
            } else {
              diagnostics.warn("email_lease_lost", {
                outbox_id: lease.id,
                kind: lease.kind,
                attempt: lease.attempt,
              });
            }
            continue;
          }

          const delayMs = nextRetryDelay(delivery, lease.attempt, backoff, maxRetryAfterMs);
          const applied = await options.store.reschedule(lease, {
            nowMs,
            nextAttemptMs: nowMs + delayMs,
            reason: delivery.reason,
          });
          if (applied) {
            incrementResult(outcome, "retried");
            diagnostics.warn("email_retry_scheduled", {
              outbox_id: lease.id,
              kind: lease.kind,
              attempt: lease.attempt,
              reason: delivery.reason,
              delay_ms: delayMs,
            });
          } else {
            diagnostics.warn("email_lease_lost", {
              outbox_id: lease.id,
              kind: lease.kind,
              attempt: lease.attempt,
            });
          }
        }

        if (leases.length < batchSize || stopped) break;
      }
      return outcome;
    })();

    activeRun = run;
    try {
      return await run;
    } finally {
      activeRun = null;
    }
  };

  const schedule = (delayMs: number): void => {
    if (stopped || !started) return;
    timer = clock.setTimeout(() => {
      timer = null;
      void runOnce().then(
        () => schedule(pollIntervalMs),
        () => schedule(pollIntervalMs),
      );
    }, delayMs);
    unrefTimer(timer);
  };

  return {
    start(): void {
      if (started) return;
      stopped = false;
      started = true;
      schedule(0);
    },

    async stop(): Promise<void> {
      stopped = true;
      started = false;
      if (timer !== null) {
        clock.clearTimeout(timer);
        timer = null;
      }
      if (activeRun) await activeRun;
    },

    runOnce,
  };
}
