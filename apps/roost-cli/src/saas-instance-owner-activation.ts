/**
 * Owns the managed instance's pending owner-activation lifecycle and email release.
 * Hidden CLI actions call these functions after migrations establish the expected schema.
 * Transactions bind the activation hash, identity, and outbox row so retries cannot fork state.
 */

import type { Database } from "bun:sqlite";
import { createHash, randomBytes, randomUUID } from "node:crypto";
import { createEmailOutboxPayloadCipher } from "@roost/shared/email-payload";
import {
  checkedOwnerActivationTime,
  countInstanceRows,
  HELD_OUTBOX_TIME_MS,
  normalizedOwnerActivationInput,
  OWNER_ACTIVATION_KIND,
  OWNER_ACTIVATION_TTL_MS,
  requireEmptySeedTables,
  withInstanceTransaction,
} from "./saas-instance-seed-database.ts";
import type {
  OwnerActivationIdentity,
  SeedOwnerActivationInput,
  SeedOwnerActivationOptions,
  SeedOwnerActivationResult,
  SeedSignupGatewayOwnerActivationOptions,
} from "./saas-instance-types.ts";

const SHA256_HEX_RE = /^[0-9a-f]{64}$/;
const RELEASED_OUTBOX_STATES: Record<string, true> = {
  sending: true,
  sent: true,
};

interface OwnerActivationRow {
  coordinator_id: string;
  account_id: string;
  email_normalized: string;
  outbox_id: string | null;
  delivery: string;
  expires_at_ms: number;
  accepted_at_ms: number | null;
  revoked_at_ms: number | null;
}

interface OwnerActivationOutboxRow {
  id: string;
  kind: string;
  state: string;
  next_attempt_ms: number;
}

function activationLink(webPublicUrl: string, tenantRouteKey: string, token: string): string {
  const url = new URL(`/activate/${tenantRouteKey}`, webPublicUrl);
  url.hash = token;
  return url.toString();
}

/** Atomically supersede the pending activation and its outbox row. Token
 * plaintext exists only long enough to render and encrypt the email payload;
 * this function neither returns nor logs it. */
export function seedOwnerActivation(
  sqlite: Database,
  rawInput: SeedOwnerActivationInput,
  options: SeedOwnerActivationOptions,
): SeedOwnerActivationResult {
  const input = normalizedOwnerActivationInput(rawInput);
  const timestamp = checkedOwnerActivationTime(options.now ?? Date.now);
  const expiresAtMs = timestamp + OWNER_ACTIVATION_TTL_MS;
  const outboxId = (options.createId ?? randomUUID)();
  if (!outboxId) throw new Error("owner activation ID generator returned an empty ID");

  const generatedBytes = options.createTokenBytes?.() ?? randomBytes(32);
  if (!(generatedBytes instanceof Uint8Array) || generatedBytes.byteLength !== 32) {
    throw new Error("owner activation token generator must return exactly 32 bytes");
  }
  const tokenBytes = Buffer.from(generatedBytes);
  let token = tokenBytes.toString("base64url");
  tokenBytes.fill(0);
  const tokenHash = createHash("sha256").update(token, "utf8").digest("hex");
  const link = activationLink(options.webPublicUrl, options.tenantRouteKey, token);
  const cipher = createEmailOutboxPayloadCipher(options.emailOutboxKey);
  const encryptedPayload = cipher.encrypt(
    { outboxId, kind: OWNER_ACTIVATION_KIND },
    {
      subject: "Activate your Roost account",
      html: `<p>Activate your Roost account.</p><p><a href="${link}">Activate account</a></p>`,
      text: `Activate your Roost account: ${link}`,
    },
  );
  token = "";

  return withInstanceTransaction(sqlite, "BEGIN IMMEDIATE", () => {
    if (countInstanceRows(sqlite, "accounts") !== 0) {
      throw new Error("refusing owner activation seed because an account already exists");
    }

    const existing = sqlite.query<OwnerActivationRow, []>(
      `SELECT coordinator_id, account_id, email_normalized, outbox_id, delivery,
              expires_at_ms, accepted_at_ms, revoked_at_ms
       FROM owner_activation_tokens`,
    ).all();
    if (existing.length > 1) {
      throw new Error("refusing owner activation seed because activation state is ambiguous");
    }
    const previous = existing[0];
    if (previous && (
      previous.coordinator_id !== input.coordinatorId
      || previous.account_id !== input.accountId
      || previous.email_normalized !== input.email
      || previous.delivery !== "coordinator-email"
      || previous.outbox_id === null
      || previous.accepted_at_ms !== null
    )) {
      throw new Error("refusing owner activation seed because activation identity does not match");
    }

    // Removing both rows in the same write transaction makes every previous
    // hash permanently unredeemable even if its email had already escaped.
    sqlite.query("DELETE FROM owner_activation_tokens").run();
    sqlite.query("DELETE FROM email_outbox WHERE kind = ?").run(OWNER_ACTIVATION_KIND);
    sqlite.query(
      `INSERT INTO email_outbox
        (id, kind, recipient, encrypted_payload, idempotency_key, state, attempts,
         locked_until_ms, lease_token, next_attempt_ms, provider_message_id,
         sent_at_ms, failed_at_ms, last_error)
       VALUES (?, ?, ?, ?, ?, 'pending', 0, NULL, NULL, ?, NULL, NULL, NULL, NULL)`,
    ).run(
      outboxId,
      OWNER_ACTIVATION_KIND,
      input.email,
      encryptedPayload,
      outboxId,
      HELD_OUTBOX_TIME_MS,
    );
    sqlite.query(
      `INSERT INTO owner_activation_tokens
        (coordinator_id, account_id, email_normalized, token_hash, outbox_id,
         delivery, created_at_ms, expires_at_ms, accepted_at_ms, revoked_at_ms)
       VALUES (?, ?, ?, ?, ?, 'coordinator-email', ?, ?, NULL, NULL)`,
    ).run(
      input.coordinatorId,
      input.accountId,
      input.email,
      tokenHash,
      outboxId,
      timestamp,
      expiresAtMs,
    );

    return {
      accountId: input.accountId,
      coordinatorId: input.coordinatorId,
      expiresAtMs,
    };
  });
}

export function seedSignupGatewayOwnerActivation(
  sqlite: Database,
  rawInput: SeedOwnerActivationInput,
  activationTokenHash: string,
  options: SeedSignupGatewayOwnerActivationOptions = {},
): SeedOwnerActivationResult {
  const input = normalizedOwnerActivationInput(rawInput);
  if (!SHA256_HEX_RE.test(activationTokenHash)) {
    throw new Error("signup-gateway activation hash must be 64 lowercase hexadecimal characters");
  }
  const timestamp = checkedOwnerActivationTime(options.now ?? Date.now);
  const expiresAtMs = timestamp + OWNER_ACTIVATION_TTL_MS;
  return withInstanceTransaction(sqlite, "BEGIN IMMEDIATE", () => {
    if (countInstanceRows(sqlite, "accounts") !== 0) {
      throw new Error("refusing signup-gateway activation seed because an account already exists");
    }
    const rows = sqlite.query<{
      coordinator_id: string;
      account_id: string;
      email_normalized: string;
      token_hash: string;
      outbox_id: string | null;
      delivery: string;
      created_at_ms: number;
      expires_at_ms: number;
      accepted_at_ms: number | null;
      revoked_at_ms: number | null;
    }, []>(`
      SELECT coordinator_id, account_id, email_normalized, token_hash, outbox_id,
             delivery, created_at_ms, expires_at_ms, accepted_at_ms, revoked_at_ms
      FROM owner_activation_tokens
      LIMIT 2
    `).all();
    if (rows.length === 1) {
      const existing = rows[0]!;
      if (
        existing.coordinator_id !== input.coordinatorId
        || existing.account_id !== input.accountId
        || existing.email_normalized !== input.email
        || existing.token_hash !== activationTokenHash
        || existing.outbox_id !== null
        || existing.delivery !== "signup-gateway"
        || existing.accepted_at_ms !== null
        || existing.revoked_at_ms !== null
      ) {
        throw new Error("refusing signup-gateway activation seed because topology differs");
      }
      requireEmptySeedTables(sqlite, [
        "account_identities",
        "account_devices",
        "organizations",
        "organization_memberships",
        "dashboards",
        "dashboard_memberships",
        "authorized_keys",
        "email_outbox",
        "password_reset_tokens",
        "federated_assertion_redemptions",
      ]);
      return {
        accountId: input.accountId,
        coordinatorId: input.coordinatorId,
        expiresAtMs: existing.expires_at_ms,
      };
    }
    if (rows.length !== 0) {
      throw new Error("refusing signup-gateway activation seed because activation state is ambiguous");
    }
    requireEmptySeedTables(sqlite, [
      "account_identities",
      "account_devices",
      "organizations",
      "organization_memberships",
      "dashboards",
      "dashboard_memberships",
      "authorized_keys",
      "email_outbox",
      "password_reset_tokens",
      "federated_assertion_redemptions",
    ]);
    sqlite.query(`
      INSERT INTO owner_activation_tokens (
        coordinator_id, account_id, email_normalized, token_hash, outbox_id,
        delivery, created_at_ms, expires_at_ms, accepted_at_ms, revoked_at_ms
      ) VALUES (?, ?, ?, ?, NULL, 'signup-gateway', ?, ?, NULL, NULL)
    `).run(
      input.coordinatorId,
      input.accountId,
      input.email,
      activationTokenHash,
      timestamp,
      expiresAtMs,
    );
    return {
      accountId: input.accountId,
      coordinatorId: input.coordinatorId,
      expiresAtMs,
    };
  });
}

/** Release only the one live activation's referenced message. Repeated calls
 * never postpone a message that is already due and never resurrect terminal
 * outbox state. */
export function releaseOwnerActivationEmail(
  sqlite: Database,
  now: () => number = Date.now,
  expectedCoordinatorId?: string,
): OwnerActivationIdentity {
  const timestamp = checkedOwnerActivationTime(now);
  return withInstanceTransaction(sqlite, "BEGIN IMMEDIATE", () => {
    if (countInstanceRows(sqlite, "accounts") !== 0) {
      throw new Error("refusing owner activation release because an account already exists");
    }
    const activations = sqlite.query<OwnerActivationRow, []>(
      `SELECT coordinator_id, account_id, email_normalized, outbox_id, delivery,
              expires_at_ms, accepted_at_ms, revoked_at_ms
       FROM owner_activation_tokens`,
    ).all();
    const activation = activations[0];
    if (
      activations.length !== 1
      || !activation
      || activation.delivery !== "coordinator-email"
      || activation.outbox_id === null
      || activation.accepted_at_ms !== null
      || activation.revoked_at_ms !== null
      || activation.expires_at_ms <= timestamp
    ) {
      throw new Error("no live owner activation is available for release");
    }
    if (
      expectedCoordinatorId !== undefined
      && activation.coordinator_id !== expectedCoordinatorId
    ) {
      throw new Error("activation coordinator ID does not match managed instance configuration");
    }

    const outbox = sqlite.query<OwnerActivationOutboxRow, [string]>(
      "SELECT id, kind, state, next_attempt_ms FROM email_outbox WHERE id = ?",
    ).get(activation.outbox_id);
    if (!outbox || outbox.kind !== OWNER_ACTIVATION_KIND) {
      throw new Error("owner activation outbox state is missing or inconsistent");
    }
    if (outbox.state === "pending") {
      sqlite.query(
        `UPDATE email_outbox
         SET next_attempt_ms = CASE WHEN next_attempt_ms > ? THEN ? ELSE next_attempt_ms END
         WHERE id = ? AND kind = ? AND state = 'pending'`,
      ).run(timestamp, timestamp, activation.outbox_id, OWNER_ACTIVATION_KIND);
    } else if (!RELEASED_OUTBOX_STATES[outbox.state]) {
      throw new Error("owner activation outbox state is invalid");
    }

    return {
      accountId: activation.account_id,
      coordinatorId: activation.coordinator_id,
    };
  });
}
