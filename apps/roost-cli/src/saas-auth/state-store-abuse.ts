/**
 * Persists rate-limit buckets and Turnstile replay proofs for the signup gateway.
 * Request protocols call these operations before any account-facing work begins.
 * Durable transactions keep abuse decisions stable across restarts and concurrent requests.
 */

import { createHash } from "node:crypto";
import {
  boundedGatewayText,
  canonicalGatewayHash,
  checkedGatewayId,
  invalidGatewayState,
  MAX_WINDOW_MS,
  safeGatewayTimestamp,
  withImmediateGatewayTransaction,
} from "./state-store-database.ts";
import type { GatewayStateContext } from "./state-store-database.ts";
import {
  GATEWAY_RATE_SCOPES,
  GatewayStateError,
  MAX_DURABLE_RATE_BUCKETS,
  MAX_DURABLE_TURNSTILE_PROOFS,
  TURNSTILE_PROOF_TTL_MS,
} from "./state-store-types.ts";
import type {
  ConsumeRateBucketOptions,
  RateBucketResult,
  TurnstileVerification,
} from "./state-store-types.ts";

export function consumeGatewayRateBucket(
  context: GatewayStateContext,
  options: ConsumeRateBucketOptions,
): RateBucketResult {
  if (!GATEWAY_RATE_SCOPES.includes(options.scope)) invalidGatewayState("invalid rate bucket scope");
  boundedGatewayText(options.key, "rate bucket key", 512);
  if (!Number.isSafeInteger(options.limit) || options.limit <= 0 || options.limit > 1_000_000) {
    invalidGatewayState("invalid rate bucket limit");
  }
  if (!Number.isSafeInteger(options.windowMs) || options.windowMs <= 0 || options.windowMs > MAX_WINDOW_MS) {
    invalidGatewayState("invalid rate bucket window");
  }
  const nowMs = safeGatewayTimestamp(options.nowMs ?? context.now(), "rate bucket time");
  const windowStartMs = Math.floor(nowMs / options.windowMs) * options.windowMs;
  const expiresAtMs = windowStartMs + options.windowMs;
  const keyHash = createHash("sha256").update(options.key, "utf8").digest("hex");
  return withImmediateGatewayTransaction(context.sqlite, () => {
    context.sqlite.query("DELETE FROM rate_buckets WHERE expires_at_ms <= ?").run(nowMs);
    const existing = context.sqlite.query(
      "SELECT count, window_start_ms, expires_at_ms FROM rate_buckets WHERE scope = ? AND key_hash = ?",
    ).get(options.scope, keyHash) as {
      count: number;
      window_start_ms: number;
      expires_at_ms: number;
    } | null;
    if (existing) {
      if (existing.window_start_ms !== windowStartMs || existing.expires_at_ms !== expiresAtMs) {
        throw new GatewayStateError("gateway rate bucket row is corrupt", "corrupt");
      }
      if (existing.count >= options.limit) {
        return { allowed: false, remaining: 0, retryAtMs: expiresAtMs };
      }
      const next = existing.count + 1;
      context.sqlite.query(
        "UPDATE rate_buckets SET count = ?, updated_at_ms = ? WHERE scope = ? AND key_hash = ?",
      ).run(next, nowMs, options.scope, keyHash);
      return { allowed: true, remaining: options.limit - next, retryAtMs: expiresAtMs };
    }
    const count = context.sqlite.query("SELECT count(*) AS count FROM rate_buckets").get() as { count: number };
    if (count.count >= MAX_DURABLE_RATE_BUCKETS) {
      throw new GatewayStateError("gateway rate bucket capacity reached", "capacity");
    }
    context.sqlite.query(
      "INSERT INTO rate_buckets(scope, key_hash, window_start_ms, expires_at_ms, count, updated_at_ms) VALUES (?, ?, ?, ?, 1, ?)",
    ).run(options.scope, keyHash, windowStartMs, expiresAtMs, nowMs);
    return { allowed: true, remaining: options.limit - 1, retryAtMs: expiresAtMs };
  });
}

export function beginGatewayTurnstileVerification(
  context: GatewayStateContext,
  tokenHashRaw: string,
  idempotencyKeyRaw: string,
  nowMsRaw: number,
): TurnstileVerification {
  const tokenHash = canonicalGatewayHash(tokenHashRaw, "Turnstile token hash");
  const idempotencyKey = checkedGatewayId(idempotencyKeyRaw, "Turnstile idempotency key");
  const nowMs = safeGatewayTimestamp(nowMsRaw, "Turnstile verification time");
  return withImmediateGatewayTransaction(context.sqlite, () => {
    const existing = context.sqlite.query(
      "SELECT idempotency_key, state, expires_at_ms FROM turnstile_proofs WHERE token_hash = ?",
    ).get(tokenHash) as { idempotency_key: string; state: string; expires_at_ms: number } | null;
    if (existing) {
      return {
        idempotencyKey: existing.idempotency_key,
        mayVerify: existing.state === "verifying" && existing.expires_at_ms > nowMs,
      };
    }
    context.sqlite.query("DELETE FROM turnstile_proofs WHERE expires_at_ms <= ?").run(
      Math.max(0, nowMs - 24 * 60 * 60 * 1_000),
    );
    const count = context.sqlite.query("SELECT count(*) AS count FROM turnstile_proofs").get() as { count: number };
    if (count.count >= MAX_DURABLE_TURNSTILE_PROOFS) {
      throw new GatewayStateError("gateway Turnstile proof capacity reached", "capacity");
    }
    context.sqlite.query(`INSERT INTO turnstile_proofs(
      token_hash, idempotency_key, state, created_at_ms, expires_at_ms, verified_at_ms
    ) VALUES (?, ?, 'verifying', ?, ?, NULL)`).run(
      tokenHash,
      idempotencyKey,
      nowMs,
      nowMs + TURNSTILE_PROOF_TTL_MS,
    );
    return { idempotencyKey, mayVerify: true };
  });
}

export function markGatewayTurnstileVerified(
  context: GatewayStateContext,
  tokenHashRaw: string,
  idempotencyKeyRaw: string,
  nowMsRaw: number,
): boolean {
  const tokenHash = canonicalGatewayHash(tokenHashRaw, "Turnstile token hash");
  const idempotencyKey = checkedGatewayId(idempotencyKeyRaw, "Turnstile idempotency key");
  const nowMs = safeGatewayTimestamp(nowMsRaw, "Turnstile verification time");
  return context.sqlite.query(`UPDATE turnstile_proofs SET state = 'verified', verified_at_ms = ?
    WHERE token_hash = ? AND idempotency_key = ? AND state = 'verifying' AND expires_at_ms > ?`)
    .run(nowMs, tokenHash, idempotencyKey, nowMs).changes === 1;
}

export function markGatewayTurnstileFailed(
  context: GatewayStateContext,
  tokenHashRaw: string,
  idempotencyKeyRaw: string,
): boolean {
  const tokenHash = canonicalGatewayHash(tokenHashRaw, "Turnstile token hash");
  const idempotencyKey = checkedGatewayId(idempotencyKeyRaw, "Turnstile idempotency key");
  return context.sqlite.query(`UPDATE turnstile_proofs SET state = 'failed'
    WHERE token_hash = ? AND idempotency_key = ? AND state = 'verifying'`)
    .run(tokenHash, idempotencyKey).changes === 1;
}
