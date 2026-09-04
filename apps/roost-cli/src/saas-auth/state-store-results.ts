/**
 * Persists provisioning receipts, deferred assertion inputs, and device bindings.
 * Result polling and federation protocols use these operations through GatewayStateStore.
 * Encrypting payloads beside atomic state transitions prevents disclosure and double binding.
 */

import {
  boundedGatewayText,
  canonicalGatewayHash,
  checkedGatewayId,
  hashGatewayCapability,
  invalidGatewayState,
  MAX_RESULT_TTL_MS,
  ROUTE_OR_FINGERPRINT_RE,
  SAFE_ID_RE,
  safeGatewayTimestamp,
  withImmediateGatewayTransaction,
} from "./state-store-database.ts";
import type { GatewayStateContext } from "./state-store-database.ts";
import { GatewayStateError } from "./state-store-types.ts";
import type {
  CreateHashedResultReceiptOptions,
  CreateResultReceiptOptions,
  GatewayResultReceipt,
  GatewayResultState,
  SetResultOutcomeOptions,
} from "./state-store-types.ts";

interface RawResultReceipt {
  id: string;
  job_id: string;
  state: GatewayResultState;
  route_key: string | null;
  assertion_input_encrypted: string | null;
  assertion_encrypted: string | null;
  bound_fingerprint: string | null;
  created_at_ms: number;
  expires_at_ms: number;
}

function insertGatewayResultReceipt(
  context: GatewayStateContext,
  receiptRaw: string,
  browserCookieHashRaw: string,
  jobIdRaw: string,
  expiresAtMsRaw: number,
  nowMsRaw: number,
): void {
  const id = checkedGatewayId(context.createId(), "result receipt id");
  const receiptHash = hashGatewayCapability(receiptRaw, "result receipt");
  const browserCookieHash = canonicalGatewayHash(browserCookieHashRaw, "browser cookie hash");
  const jobId = boundedGatewayText(jobIdRaw, "provisioning job id", 256);
  if (!SAFE_ID_RE.test(jobId)) invalidGatewayState("invalid provisioning job id");
  const nowMs = safeGatewayTimestamp(nowMsRaw, "result creation time");
  const expiresAtMs = safeGatewayTimestamp(expiresAtMsRaw, "result expiry time");
  if (expiresAtMs <= nowMs || expiresAtMs - nowMs > MAX_RESULT_TTL_MS) {
    invalidGatewayState("invalid result lifetime");
  }
  context.sqlite.query(`INSERT INTO result_receipts(
    id, receipt_hash, browser_cookie_hash, job_id, state, route_key,
    assertion_input_encrypted, assertion_encrypted, bound_fingerprint,
    created_at_ms, expires_at_ms, updated_at_ms, invalidated_at_ms
  ) VALUES (?, ?, ?, ?, 'pending', NULL, NULL, NULL, NULL, ?, ?, ?, NULL)`)
    .run(id, receiptHash, browserCookieHash, jobId, nowMs, expiresAtMs, nowMs);
}

export function createGatewayResultReceipt(
  context: GatewayStateContext,
  options: CreateResultReceiptOptions,
): void {
  insertGatewayResultReceipt(
    context,
    options.receipt,
    hashGatewayCapability(options.browserCookie, "browser cookie"),
    options.jobId,
    options.expiresAtMs,
    options.nowMs ?? context.now(),
  );
}

export function createGatewayResultReceiptForBrowserHash(
  context: GatewayStateContext,
  options: CreateHashedResultReceiptOptions,
): void {
  insertGatewayResultReceipt(
    context,
    options.receipt,
    options.browserCookieHash,
    options.jobId,
    options.expiresAtMs,
    options.nowMs ?? context.now(),
  );
}

export function setGatewayResultOutcome(
  context: GatewayStateContext,
  options: SetResultOutcomeOptions,
): boolean {
  const jobId = boundedGatewayText(options.jobId, "provisioning job id", 256);
  const nowMs = safeGatewayTimestamp(options.nowMs ?? context.now(), "result update time");
  if (options.state === "awaiting-device") {
    if (!options.routeKey || !ROUTE_OR_FINGERPRINT_RE.test(options.routeKey)) {
      invalidGatewayState("invalid result route key");
    }
    const row = context.sqlite.query(
      "SELECT id, state, route_key, assertion_input_encrypted FROM result_receipts WHERE job_id = ?",
    ).get(jobId) as Pick<RawResultReceipt, "id" | "state" | "route_key" | "assertion_input_encrypted"> | null;
    if (!row) return false;
    if (row.state === "awaiting-device") {
      const stored = row.assertion_input_encrypted === null
        ? null
        : context.cipher.decrypt(row.id, "assertion-input", row.assertion_input_encrypted);
      return row.route_key === options.routeKey && stored === options.assertionInput;
    }
    if (row.state !== "pending") return false;
    const assertionInput = context.cipher.encrypt(
      row.id,
      "assertion-input",
      boundedGatewayText(options.assertionInput ?? "", "assertion input", 128 * 1024),
    );
    return context.sqlite.query(`UPDATE result_receipts SET state = 'awaiting-device', route_key = ?,
      assertion_input_encrypted = ?, updated_at_ms = ? WHERE id = ? AND state = 'pending'
      AND invalidated_at_ms IS NULL AND expires_at_ms > ?`)
      .run(options.routeKey, assertionInput, nowMs, row.id, nowMs).changes === 1;
  }
  if (options.routeKey !== undefined || options.assertionInput !== undefined) {
    invalidGatewayState("terminal result has unexpected data");
  }
  const updated = context.sqlite.query(`UPDATE result_receipts SET state = ?, updated_at_ms = ?
    WHERE job_id = ? AND state = 'pending' AND invalidated_at_ms IS NULL AND expires_at_ms > ?`)
    .run(options.state, nowMs, jobId, nowMs);
  if (updated.changes === 1) return true;
  const row = context.sqlite.query("SELECT state FROM result_receipts WHERE job_id = ?")
    .get(jobId) as { state: string } | null;
  return row?.state === options.state;
}

export function getGatewayAssertionInput(
  context: GatewayStateContext,
  receiptRaw: string,
  nowMsRaw: number,
): string | null {
  const receiptHash = hashGatewayCapability(receiptRaw, "result receipt");
  const nowMs = safeGatewayTimestamp(nowMsRaw, "result read time");
  const row = context.sqlite.query(`SELECT id, assertion_input_encrypted FROM result_receipts
    WHERE receipt_hash = ? AND state IN ('awaiting-device','ready') AND invalidated_at_ms IS NULL AND expires_at_ms > ?`)
    .get(receiptHash, nowMs) as Pick<RawResultReceipt, "id" | "assertion_input_encrypted"> | null;
  if (!row?.assertion_input_encrypted) return null;
  return context.cipher.decrypt(row.id, "assertion-input", row.assertion_input_encrypted);
}

export function bindGatewayResultAssertion(
  context: GatewayStateContext,
  receiptRaw: string,
  fingerprintRaw: string,
  assertionRaw: string,
  nowMsRaw: number,
): string | null {
  const receiptHash = hashGatewayCapability(receiptRaw, "result receipt");
  if (!ROUTE_OR_FINGERPRINT_RE.test(fingerprintRaw)) invalidGatewayState("invalid device fingerprint");
  const assertion = boundedGatewayText(assertionRaw, "federated assertion", 16 * 1024);
  const nowMs = safeGatewayTimestamp(nowMsRaw, "assertion bind time");
  return withImmediateGatewayTransaction(context.sqlite, () => {
    const row = context.sqlite.query(`SELECT id, state, bound_fingerprint, assertion_encrypted
      FROM result_receipts WHERE receipt_hash = ? AND invalidated_at_ms IS NULL AND expires_at_ms > ?`)
      .get(receiptHash, nowMs) as Pick<
        RawResultReceipt,
        "id" | "state" | "bound_fingerprint" | "assertion_encrypted"
      > | null;
    if (!row) return null;
    if (row.state === "ready") {
      if (row.bound_fingerprint !== fingerprintRaw || row.assertion_encrypted === null) {
        throw new GatewayStateError("result is bound to another device", "conflict");
      }
      return context.cipher.decrypt(row.id, "assertion", row.assertion_encrypted);
    }
    if (row.state !== "awaiting-device") return null;
    const encrypted = context.cipher.encrypt(row.id, "assertion", assertion);
    if (context.sqlite.query(`UPDATE result_receipts SET state = 'ready', bound_fingerprint = ?,
      assertion_encrypted = ?, updated_at_ms = ? WHERE id = ? AND state = 'awaiting-device'
      AND bound_fingerprint IS NULL AND assertion_encrypted IS NULL AND invalidated_at_ms IS NULL
      AND expires_at_ms > ?`).run(fingerprintRaw, encrypted, nowMs, row.id, nowMs).changes !== 1) {
      throw new GatewayStateError("result binding raced", "conflict");
    }
    return assertion;
  });
}

export function getGatewayResult(
  context: GatewayStateContext,
  receiptRaw: string,
  nowMsRaw: number,
): GatewayResultReceipt | null {
  const receiptHash = hashGatewayCapability(receiptRaw, "result receipt");
  const nowMs = safeGatewayTimestamp(nowMsRaw, "result read time");
  const row = context.sqlite.query(`SELECT id, job_id, state, route_key, assertion_input_encrypted,
    assertion_encrypted, bound_fingerprint, created_at_ms, expires_at_ms FROM result_receipts
    WHERE receipt_hash = ? AND invalidated_at_ms IS NULL AND expires_at_ms > ?`)
    .get(receiptHash, nowMs) as RawResultReceipt | null;
  if (!row) return null;
  return {
    jobId: row.job_id,
    state: row.state,
    routeKey: row.route_key,
    assertion: row.assertion_encrypted === null
      ? null
      : context.cipher.decrypt(row.id, "assertion", row.assertion_encrypted),
    boundFingerprint: row.bound_fingerprint,
    createdAtMs: row.created_at_ms,
    expiresAtMs: row.expires_at_ms,
  };
}
