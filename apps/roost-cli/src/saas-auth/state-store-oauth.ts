/**
 * Persists one-time OAuth attempts and their encrypted callback capabilities.
 * Google OAuth flows call these operations through GatewayStateStore.
 * Keeping invalidation and consumption transactional prevents replay across browser sessions.
 */

import {
  boundedGatewayText,
  checkedGatewayId,
  hashGatewayCapability,
  invalidGatewayState,
  NONCE_RE,
  PKCE_RE,
  ROUTE_OR_FINGERPRINT_RE,
  safeGatewayTimestamp,
  withImmediateGatewayTransaction,
} from "./state-store-database.ts";
import type { GatewayStateContext } from "./state-store-database.ts";
import { OAUTH_ATTEMPT_TTL_MS } from "./state-store-types.ts";
import type {
  ConsumedOAuthAttempt,
  OAuthAttemptInput,
  OAuthIntent,
} from "./state-store-types.ts";

interface RawOAuthAttempt {
  id: string;
  browser_cookie_hash: string;
  pkce_verifier_encrypted: string;
  nonce: string;
  intent: OAuthIntent;
  route_key: string | null;
  link_ticket_encrypted: string | null;
  proof_at_ms: number | null;
  created_at_ms: number;
  expires_at_ms: number;
}

export function startGatewayOAuthAttempt(
  context: GatewayStateContext,
  input: OAuthAttemptInput,
): string {
  const id = checkedGatewayId(context.createId(), "OAuth attempt id");
  const browserCookieHash = hashGatewayCapability(input.browserCookie, "browser cookie");
  const oauthCookieHash = hashGatewayCapability(input.oauthCookie, "OAuth cookie");
  const stateHash = hashGatewayCapability(input.state, "OAuth state");
  if (!PKCE_RE.test(input.pkceVerifier)) invalidGatewayState("invalid PKCE verifier");
  if (!NONCE_RE.test(input.nonce)) invalidGatewayState("invalid OAuth nonce");
  const nowMs = safeGatewayTimestamp(input.nowMs ?? context.now(), "OAuth attempt time");
  let routeKey: string | null = null;
  let linkTicketEncrypted: string | null = null;
  let proofAtMs: number | null = null;
  if (input.intent === "link") {
    if (!input.routeKey || !ROUTE_OR_FINGERPRINT_RE.test(input.routeKey)) {
      invalidGatewayState("invalid link route key");
    }
    routeKey = input.routeKey;
    linkTicketEncrypted = context.cipher.encrypt(
      id,
      "link-ticket",
      boundedGatewayText(input.linkTicket ?? "", "link ticket", 16 * 1024),
    );
    if (input.proofAtMs !== undefined) invalidGatewayState("link attempt cannot carry Turnstile proof time");
  } else if (input.intent === "signup") {
    proofAtMs = safeGatewayTimestamp(input.proofAtMs ?? -1, "Turnstile proof time");
    if (proofAtMs > nowMs || nowMs - proofAtMs > 300_000) invalidGatewayState("Turnstile proof is stale");
    if (input.routeKey !== undefined || input.linkTicket !== undefined) {
      invalidGatewayState("signup attempt has link fields");
    }
  } else if (input.intent === "login") {
    if (
      input.routeKey !== undefined
      || input.linkTicket !== undefined
      || input.proofAtMs !== undefined
    ) {
      invalidGatewayState("login attempt has unexpected fields");
    }
  } else {
    invalidGatewayState("invalid OAuth intent");
  }
  const pkceEncrypted = context.cipher.encrypt(id, "pkce-verifier", input.pkceVerifier);
  return withImmediateGatewayTransaction(context.sqlite, () => {
    context.sqlite.query(`UPDATE oauth_attempts SET invalidated_at_ms = ?
      WHERE browser_cookie_hash = ? AND consumed_at_ms IS NULL AND invalidated_at_ms IS NULL`)
      .run(nowMs, browserCookieHash);
    context.sqlite.query(`UPDATE result_receipts SET invalidated_at_ms = ?, updated_at_ms = ?
      WHERE browser_cookie_hash = ? AND invalidated_at_ms IS NULL`).run(nowMs, nowMs, browserCookieHash);
    context.sqlite.query(`INSERT INTO oauth_attempts(
      id, browser_cookie_hash, oauth_cookie_hash, state_hash, pkce_verifier_encrypted,
      nonce, intent, route_key, link_ticket_encrypted, proof_at_ms, created_at_ms,
      expires_at_ms, consumed_at_ms, invalidated_at_ms
    ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, NULL, NULL)`).run(
      id,
      browserCookieHash,
      oauthCookieHash,
      stateHash,
      pkceEncrypted,
      input.nonce,
      input.intent,
      routeKey,
      linkTicketEncrypted,
      proofAtMs,
      nowMs,
      nowMs + OAUTH_ATTEMPT_TTL_MS,
    );
    return id;
  });
}

export function consumeGatewayOAuthAttempt(
  context: GatewayStateContext,
  oauthCookieRaw: string,
  stateRaw: string,
  nowMsRaw: number,
): ConsumedOAuthAttempt | null {
  const oauthCookieHash = hashGatewayCapability(oauthCookieRaw, "OAuth cookie");
  const stateHash = hashGatewayCapability(stateRaw, "OAuth state");
  const nowMs = safeGatewayTimestamp(nowMsRaw, "OAuth callback time");
  return withImmediateGatewayTransaction(context.sqlite, () => {
    const row = context.sqlite.query(`SELECT id, browser_cookie_hash, pkce_verifier_encrypted,
      nonce, intent, route_key, link_ticket_encrypted, proof_at_ms, created_at_ms, expires_at_ms
      FROM oauth_attempts WHERE oauth_cookie_hash = ? AND state_hash = ?
        AND consumed_at_ms IS NULL AND invalidated_at_ms IS NULL AND expires_at_ms > ?`)
      .get(oauthCookieHash, stateHash, nowMs) as RawOAuthAttempt | null;
    if (!row) return null;
    if (context.sqlite.query(`UPDATE oauth_attempts SET consumed_at_ms = ?
      WHERE id = ? AND consumed_at_ms IS NULL AND invalidated_at_ms IS NULL`)
      .run(nowMs, row.id).changes !== 1) {
      return null;
    }
    return {
      id: row.id,
      intent: row.intent,
      pkceVerifier: context.cipher.decrypt(row.id, "pkce-verifier", row.pkce_verifier_encrypted),
      nonce: row.nonce,
      routeKey: row.route_key,
      linkTicket: row.link_ticket_encrypted === null
        ? null
        : context.cipher.decrypt(row.id, "link-ticket", row.link_ticket_encrypted),
      proofAtMs: row.proof_at_ms,
      createdAtMs: row.created_at_ms,
      expiresAtMs: row.expires_at_ms,
      browserCookieHash: row.browser_cookie_hash,
    };
  });
}
