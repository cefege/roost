/**
 * Persists signup email challenges and leases for the gateway's delivery loop.
 * The email protocol and outbox call these operations through GatewayStateStore.
 * Coupling challenge and outbox writes here preserves their atomic delivery lifecycle.
 */

import { randomUUID } from "node:crypto";
import { normalizeAccountEmail } from "@roost/shared/native-credentials";
import {
  boundedGatewayText,
  canonicalGatewayHash,
  checkedGatewayId,
  invalidGatewayState,
  MAX_CLAIM_LIMIT,
  MAX_ENCRYPTED_EMAIL_BYTES,
  MAX_LEASE_DURATION_MS,
  safeGatewayTimestamp,
  withImmediateGatewayTransaction,
} from "./state-store-database.ts";
import type { GatewayStateContext } from "./state-store-database.ts";
import {
  EMAIL_CHALLENGE_TTL_MS,
  GatewayStateError,
} from "./state-store-types.ts";
import type {
  ClaimSignupEmailOptions,
  CreateEmailChallengeOptions,
  GatewayEmailChallenge,
  SignupEmailLease,
} from "./state-store-types.ts";

interface RawEmailChallenge {
  id: string;
  token_hash: string;
  email_normalized: string;
  state: string;
  created_at_ms: number;
  expires_at_ms: number;
  verified_at_ms: number | null;
  consumed_at_ms: number | null;
  outbox_id: string;
}

interface RawSignupEmailLease {
  id: string;
  challenge_id: string;
  recipient: string;
  encrypted_payload: string;
  attempts: number;
  lease_token: string;
}

function mapEmailChallenge(row: RawEmailChallenge): GatewayEmailChallenge {
  try {
    checkedGatewayId(row.id, "email challenge row id");
    checkedGatewayId(row.outbox_id, "email challenge row outbox id");
    canonicalGatewayHash(row.token_hash, "email challenge row token hash");
  } catch {
    throw new GatewayStateError("gateway email challenge row is corrupt", "corrupt");
  }
  return {
    id: row.id,
    tokenHash: row.token_hash,
    emailNormalized: row.email_normalized,
    state: row.state as GatewayEmailChallenge["state"],
    createdAtMs: row.created_at_ms,
    expiresAtMs: row.expires_at_ms,
    verifiedAtMs: row.verified_at_ms,
    consumedAtMs: row.consumed_at_ms,
    outboxId: row.outbox_id,
  };
}

export function createGatewayEmailChallenge(
  context: GatewayStateContext,
  options: CreateEmailChallengeOptions,
): GatewayEmailChallenge {
  const id = checkedGatewayId(options.id ?? context.createId(), "email challenge id");
  const outboxId = checkedGatewayId(options.outboxId ?? context.createId(), "email outbox id");
  const tokenHash = canonicalGatewayHash(options.tokenHash, "email challenge token hash");
  const normalized = normalizeAccountEmail(options.emailNormalized);
  if (!normalized || normalized !== options.emailNormalized) invalidGatewayState("invalid normalized email");
  boundedGatewayText(options.encryptedPayload, "encrypted email payload", MAX_ENCRYPTED_EMAIL_BYTES);
  const nowMs = safeGatewayTimestamp(options.nowMs ?? context.now(), "email challenge time");
  const expiresAtMs = nowMs + EMAIL_CHALLENGE_TTL_MS;
  return withImmediateGatewayTransaction(context.sqlite, () => {
    context.sqlite.query(`INSERT INTO email_challenges(
      id, token_hash, email_normalized, state, created_at_ms, expires_at_ms,
      verified_at_ms, consumed_at_ms, outbox_id
    ) VALUES (?, ?, ?, 'pending', ?, ?, NULL, NULL, ?)`).run(
      id,
      tokenHash,
      normalized,
      nowMs,
      expiresAtMs,
      outboxId,
    );
    context.sqlite.query(`INSERT INTO signup_email_outbox(
      id, challenge_id, recipient, encrypted_payload, state, attempts,
      next_attempt_ms, locked_until_ms, lease_token, provider_message_id,
      last_error, created_at_ms, sent_at_ms, failed_at_ms
    ) VALUES (?, ?, ?, ?, 'pending', 0, ?, NULL, NULL, NULL, NULL, ?, NULL, NULL)`)
      .run(outboxId, id, normalized, options.encryptedPayload, nowMs, nowMs);
    return mapEmailChallenge(
      context.sqlite.query("SELECT * FROM email_challenges WHERE id = ?").get(id) as RawEmailChallenge,
    );
  });
}

export function verifyGatewayEmailChallenge(
  context: GatewayStateContext,
  tokenHashRaw: string,
  nowMsRaw: number,
): GatewayEmailChallenge | null {
  const tokenHash = canonicalGatewayHash(tokenHashRaw, "email challenge token hash");
  const nowMs = safeGatewayTimestamp(nowMsRaw, "email challenge verification time");
  return withImmediateGatewayTransaction(context.sqlite, () => {
    const row = context.sqlite.query("SELECT * FROM email_challenges WHERE token_hash = ?")
      .get(tokenHash) as RawEmailChallenge | null;
    if (!row || row.expires_at_ms <= nowMs || row.state === "consumed") return null;
    if (row.state === "pending") {
      context.sqlite.query(
        "UPDATE email_challenges SET state = 'verified', verified_at_ms = ? WHERE id = ? AND state = 'pending'",
      ).run(nowMs, row.id);
    }
    return mapEmailChallenge(
      context.sqlite.query("SELECT * FROM email_challenges WHERE id = ?").get(row.id) as RawEmailChallenge,
    );
  });
}

export function consumeGatewayEmailChallenge(
  context: GatewayStateContext,
  idRaw: string,
  nowMsRaw: number,
): boolean {
  const id = checkedGatewayId(idRaw, "email challenge id");
  const nowMs = safeGatewayTimestamp(nowMsRaw, "email challenge consumption time");
  return context.sqlite.query(`UPDATE email_challenges SET state = 'consumed', consumed_at_ms = ?
    WHERE id = ? AND state = 'verified' AND expires_at_ms > ?`).run(nowMs, id, nowMs).changes === 1;
}

export function claimDueGatewaySignupEmails(
  context: GatewayStateContext,
  options: ClaimSignupEmailOptions,
): SignupEmailLease[] {
  const nowMs = safeGatewayTimestamp(options.nowMs ?? context.now(), "email claim time");
  if (
    !Number.isSafeInteger(options.leaseDurationMs)
    || options.leaseDurationMs <= 0
    || options.leaseDurationMs > MAX_LEASE_DURATION_MS
  ) {
    invalidGatewayState("invalid email lease duration");
  }
  if (!Number.isSafeInteger(options.limit) || options.limit <= 0 || options.limit > MAX_CLAIM_LIMIT) {
    invalidGatewayState("invalid email claim limit");
  }
  const leaseToken = randomUUID();
  const rows = context.sqlite.query(`UPDATE signup_email_outbox
    SET state = 'sending', locked_until_ms = ?, lease_token = ?, attempts = attempts + 1
    WHERE id IN (SELECT id FROM signup_email_outbox
      WHERE (state = 'pending' AND next_attempt_ms <= ?)
         OR (state = 'sending' AND locked_until_ms <= ?)
      ORDER BY next_attempt_ms, id LIMIT ?)
    RETURNING id, challenge_id, recipient, encrypted_payload, attempts, lease_token`)
    .all(nowMs + options.leaseDurationMs, leaseToken, nowMs, nowMs, options.limit) as RawSignupEmailLease[];
  return rows.map((row) => ({
    id: row.id,
    challengeId: row.challenge_id,
    recipient: row.recipient,
    encryptedPayload: row.encrypted_payload,
    attempt: row.attempts,
    leaseToken: row.lease_token,
  }));
}

export function markGatewaySignupEmailSent(
  context: GatewayStateContext,
  lease: SignupEmailLease,
  providerMessageIdRaw: string,
  nowMsRaw: number,
): boolean {
  const providerMessageId = boundedGatewayText(providerMessageIdRaw, "provider message id", 512);
  const nowMs = safeGatewayTimestamp(nowMsRaw, "email sent time");
  return context.sqlite.query(`UPDATE signup_email_outbox SET state = 'sent', locked_until_ms = NULL,
    lease_token = NULL, provider_message_id = ?, last_error = NULL, sent_at_ms = ?, failed_at_ms = NULL
    WHERE id = ? AND state = 'sending' AND lease_token = ?`)
    .run(providerMessageId, nowMs, lease.id, lease.leaseToken).changes === 1;
}

export function rescheduleGatewaySignupEmail(
  context: GatewayStateContext,
  lease: SignupEmailLease,
  nextAttemptMsRaw: number,
  reasonRaw: string,
): boolean {
  const nextAttemptMs = safeGatewayTimestamp(nextAttemptMsRaw, "next email attempt time");
  const reason = boundedGatewayText(reasonRaw, "email retry reason", 128);
  return context.sqlite.query(`UPDATE signup_email_outbox SET state = 'pending', locked_until_ms = NULL,
    lease_token = NULL, next_attempt_ms = ?, last_error = ?
    WHERE id = ? AND state = 'sending' AND lease_token = ?`)
    .run(nextAttemptMs, reason, lease.id, lease.leaseToken).changes === 1;
}

export function failGatewaySignupEmail(
  context: GatewayStateContext,
  lease: SignupEmailLease,
  reasonRaw: string,
  nowMsRaw: number,
): boolean {
  const reason = boundedGatewayText(reasonRaw, "email failure reason", 128);
  const nowMs = safeGatewayTimestamp(nowMsRaw, "email failure time");
  return context.sqlite.query(`UPDATE signup_email_outbox SET state = 'failed', locked_until_ms = NULL,
    lease_token = NULL, last_error = ?, failed_at_ms = ?, sent_at_ms = NULL
    WHERE id = ? AND state = 'sending' AND lease_token = ?`)
    .run(reason, nowMs, lease.id, lease.leaseToken).changes === 1;
}
