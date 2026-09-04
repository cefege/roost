/**
 * Opens and initializes the signup gateway's encrypted SQLite state database.
 * Domain-specific state operations share this context so transactions and validation stay uniform.
 * The schema and cipher remain centralized because their invariants span every state family.
 */

import { Database } from "bun:sqlite";
import {
  createCipheriv,
  createDecipheriv,
  createHash,
  randomBytes,
  randomUUID,
} from "node:crypto";
import { chmodSync, mkdirSync } from "node:fs";
import { dirname, resolve } from "node:path";
import {
  EMAIL_CHALLENGE_TTL_MS,
  GatewayStateError,
  OAUTH_ATTEMPT_TTL_MS,
  TURNSTILE_PROOF_TTL_MS,
} from "./state-store-types.ts";
import type { OpenGatewayStateStoreOptions } from "./state-store-types.ts";

const DEFAULT_STATE_PATH = "/var/lib/roost-signup/auth.db";
const SHA256_RE = /^[0-9a-f]{64}$/;
const UUID_RE = /^[0-9a-f]{8}-[0-9a-f]{4}-[1-5][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;
const CAPABILITY_RE = /^[A-Za-z0-9_-]{22,256}$/;
const AES_IV_BYTES = 12;
const AES_TAG_BYTES = 16;
const MAX_ENCRYPTED_VALUE_BYTES = 256 * 1024;

export const MAX_RESULT_TTL_MS = EMAIL_CHALLENGE_TTL_MS;
export const ROUTE_OR_FINGERPRINT_RE = /^[0-9a-f]{64}$/;
export const PKCE_RE = /^[A-Za-z0-9._~-]{43,128}$/;
export const NONCE_RE = /^[A-Za-z0-9_-]{22,256}$/;
export const SAFE_ID_RE = /^[A-Za-z0-9][A-Za-z0-9._:-]{0,255}$/;
export const MAX_ENCRYPTED_EMAIL_BYTES = 2 * 1024 * 1024;
export const MAX_LEASE_DURATION_MS = 24 * 60 * 60 * 1_000;
export const MAX_CLAIM_LIMIT = 100;
export const MAX_WINDOW_MS = 24 * 60 * 60 * 1_000;

export interface GatewayStateContext {
  readonly path: string;
  readonly sqlite: Database;
  readonly now: () => number;
  readonly createId: () => string;
  readonly cipher: GatewayStateCipher;
}

export function invalidGatewayState(message: string): never {
  throw new GatewayStateError(message, "invalid");
}

export function safeGatewayTimestamp(value: number, name: string): number {
  if (!Number.isSafeInteger(value) || value < 0) invalidGatewayState(`invalid ${name}`);
  return value;
}

export function boundedGatewayText(value: string, name: string, maxBytes: number): string {
  if (value.length === 0 || Buffer.byteLength(value, "utf8") > maxBytes) {
    invalidGatewayState(`invalid ${name}`);
  }
  return value;
}

export function hashGatewayCapability(value: string, name: string): string {
  if (!CAPABILITY_RE.test(value)) invalidGatewayState(`invalid ${name}`);
  return createHash("sha256").update(value, "utf8").digest("hex");
}

export function canonicalGatewayHash(value: string, name: string): string {
  if (!SHA256_RE.test(value)) invalidGatewayState(`invalid ${name}`);
  return value;
}

export function checkedGatewayId(value: string, name: string): string {
  if (!UUID_RE.test(value)) invalidGatewayState(`invalid ${name}`);
  return value;
}

export function withImmediateGatewayTransaction<T>(sqlite: Database, action: () => T): T {
  sqlite.exec("BEGIN IMMEDIATE");
  try {
    const value = action();
    sqlite.exec("COMMIT");
    return value;
  } catch (error) {
    try {
      sqlite.exec("ROLLBACK");
    } catch {
      // Preserve the failure that required the rollback.
    }
    throw error;
  }
}

export class GatewayStateCipher {
  private readonly key: Buffer;

  constructor(encodedKey: string) {
    if (!/^[A-Za-z0-9_-]{43}$/.test(encodedKey)) invalidGatewayState("invalid OAuth state key");
    const key = Buffer.from(encodedKey, "base64url");
    if (key.byteLength !== 32 || key.toString("base64url") !== encodedKey) {
      invalidGatewayState("invalid OAuth state key");
    }
    this.key = key;
  }

  encrypt(rowId: string, field: string, plaintext: string): string {
    boundedGatewayText(plaintext, field, 128 * 1024);
    const iv = randomBytes(AES_IV_BYTES);
    const cipher = createCipheriv("aes-256-gcm", this.key, iv, { authTagLength: AES_TAG_BYTES });
    cipher.setAAD(Buffer.from(`roost-saas-auth-state:v1:${rowId}:${field}`, "utf8"));
    const ciphertext = Buffer.concat([cipher.update(plaintext, "utf8"), cipher.final()]);
    const tag = cipher.getAuthTag();
    return `v1.${iv.toString("base64url")}.${tag.toString("base64url")}.${ciphertext.toString("base64url")}`;
  }

  decrypt(rowId: string, field: string, envelope: string): string {
    if (Buffer.byteLength(envelope, "utf8") > MAX_ENCRYPTED_VALUE_BYTES) {
      throw new GatewayStateError("encrypted gateway state is invalid", "corrupt");
    }
    const parts = envelope.split(".");
    if (parts.length !== 4 || parts[0] !== "v1") {
      throw new GatewayStateError("encrypted gateway state is invalid", "corrupt");
    }
    try {
      const iv = Buffer.from(parts[1]!, "base64url");
      const tag = Buffer.from(parts[2]!, "base64url");
      const ciphertext = Buffer.from(parts[3]!, "base64url");
      if (
        iv.byteLength !== AES_IV_BYTES
        || tag.byteLength !== AES_TAG_BYTES
        || ciphertext.byteLength === 0
        || iv.toString("base64url") !== parts[1]
        || tag.toString("base64url") !== parts[2]
        || ciphertext.toString("base64url") !== parts[3]
      ) {
        throw new Error("invalid envelope");
      }
      const decipher = createDecipheriv("aes-256-gcm", this.key, iv, { authTagLength: AES_TAG_BYTES });
      decipher.setAAD(Buffer.from(`roost-saas-auth-state:v1:${rowId}:${field}`, "utf8"));
      decipher.setAuthTag(tag);
      return Buffer.concat([decipher.update(ciphertext), decipher.final()]).toString("utf8");
    } catch {
      throw new GatewayStateError("encrypted gateway state is invalid", "corrupt");
    }
  }
}

function initializeGatewayStateDatabase(sqlite: Database): void {
  sqlite.exec("PRAGMA foreign_keys=ON");
  sqlite.exec("PRAGMA busy_timeout=5000");
  sqlite.exec("PRAGMA journal_mode=WAL");
  sqlite.exec("PRAGMA secure_delete=ON");
  withImmediateGatewayTransaction(sqlite, () => {
    const version = sqlite.query("PRAGMA user_version").get() as { user_version: number };
    if (version.user_version !== 0 && version.user_version !== 1) {
      throw new GatewayStateError("gateway state schema version is unsupported", "corrupt");
    }
    sqlite.exec(`
      CREATE TABLE IF NOT EXISTS rate_buckets (
        scope TEXT NOT NULL CHECK(scope IN ('email-start-ip','email-start-email','google-signup-ip','google-login-ip')),
        key_hash TEXT NOT NULL CHECK(length(key_hash) = 64 AND key_hash NOT GLOB '*[^0-9a-f]*'),
        window_start_ms INTEGER NOT NULL CHECK(window_start_ms >= 0),
        expires_at_ms INTEGER NOT NULL CHECK(expires_at_ms > window_start_ms),
        count INTEGER NOT NULL CHECK(count BETWEEN 1 AND 1000000),
        updated_at_ms INTEGER NOT NULL CHECK(updated_at_ms >= window_start_ms),
        PRIMARY KEY(scope, key_hash)
      );

      CREATE TABLE IF NOT EXISTS turnstile_proofs (
        token_hash TEXT PRIMARY KEY CHECK(length(token_hash) = 64 AND token_hash NOT GLOB '*[^0-9a-f]*'),
        idempotency_key TEXT NOT NULL UNIQUE,
        state TEXT NOT NULL CHECK(state IN ('verifying','verified','failed')),
        created_at_ms INTEGER NOT NULL CHECK(created_at_ms >= 0),
        expires_at_ms INTEGER NOT NULL CHECK(expires_at_ms - created_at_ms = ${TURNSTILE_PROOF_TTL_MS}),
        verified_at_ms INTEGER,
        CHECK((state = 'verifying' AND verified_at_ms IS NULL)
          OR (state = 'verified' AND verified_at_ms IS NOT NULL)
          OR (state = 'failed' AND verified_at_ms IS NULL))
      );
      CREATE INDEX IF NOT EXISTS turnstile_proofs_expiry ON turnstile_proofs(expires_at_ms);
      CREATE INDEX IF NOT EXISTS rate_buckets_expiry ON rate_buckets(expires_at_ms);

      CREATE TABLE IF NOT EXISTS email_challenges (
        id TEXT PRIMARY KEY,
        token_hash TEXT NOT NULL UNIQUE CHECK(length(token_hash) = 64 AND token_hash NOT GLOB '*[^0-9a-f]*'),
        email_normalized TEXT NOT NULL CHECK(length(CAST(email_normalized AS BLOB)) BETWEEN 3 AND 320),
        state TEXT NOT NULL CHECK(state IN ('pending','verified','consumed')),
        created_at_ms INTEGER NOT NULL CHECK(created_at_ms >= 0),
        expires_at_ms INTEGER NOT NULL CHECK(expires_at_ms - created_at_ms = ${EMAIL_CHALLENGE_TTL_MS}),
        verified_at_ms INTEGER,
        consumed_at_ms INTEGER,
        outbox_id TEXT NOT NULL UNIQUE,
        CHECK((state = 'pending' AND verified_at_ms IS NULL AND consumed_at_ms IS NULL)
          OR (state = 'verified' AND verified_at_ms IS NOT NULL AND consumed_at_ms IS NULL)
          OR (state = 'consumed' AND verified_at_ms IS NOT NULL AND consumed_at_ms IS NOT NULL)),
        CHECK(verified_at_ms IS NULL OR verified_at_ms BETWEEN created_at_ms AND expires_at_ms),
        CHECK(consumed_at_ms IS NULL OR consumed_at_ms BETWEEN verified_at_ms AND expires_at_ms)
      );

      CREATE TABLE IF NOT EXISTS signup_email_outbox (
        id TEXT PRIMARY KEY,
        challenge_id TEXT NOT NULL UNIQUE REFERENCES email_challenges(id) ON DELETE RESTRICT,
        kind TEXT NOT NULL DEFAULT 'signup-verification' CHECK(kind = 'signup-verification'),
        recipient TEXT NOT NULL CHECK(length(CAST(recipient AS BLOB)) BETWEEN 3 AND 320),
        encrypted_payload TEXT NOT NULL CHECK(length(CAST(encrypted_payload AS BLOB)) BETWEEN 1 AND ${MAX_ENCRYPTED_EMAIL_BYTES}),
        state TEXT NOT NULL CHECK(state IN ('pending','sending','sent','failed')),
        attempts INTEGER NOT NULL DEFAULT 0 CHECK(attempts BETWEEN 0 AND 1000),
        next_attempt_ms INTEGER NOT NULL CHECK(next_attempt_ms >= 0),
        locked_until_ms INTEGER,
        lease_token TEXT,
        provider_message_id TEXT CHECK(provider_message_id IS NULL OR length(CAST(provider_message_id AS BLOB)) BETWEEN 1 AND 512),
        last_error TEXT CHECK(last_error IS NULL OR length(CAST(last_error AS BLOB)) BETWEEN 1 AND 128),
        created_at_ms INTEGER NOT NULL CHECK(created_at_ms >= 0),
        sent_at_ms INTEGER,
        failed_at_ms INTEGER,
        CHECK((state = 'sending' AND locked_until_ms IS NOT NULL AND lease_token IS NOT NULL)
          OR (state != 'sending' AND locked_until_ms IS NULL AND lease_token IS NULL)),
        CHECK((state = 'sent' AND sent_at_ms IS NOT NULL AND failed_at_ms IS NULL)
          OR (state = 'failed' AND failed_at_ms IS NOT NULL AND sent_at_ms IS NULL)
          OR (state IN ('pending','sending') AND sent_at_ms IS NULL AND failed_at_ms IS NULL))
      );
      CREATE INDEX IF NOT EXISTS signup_email_outbox_due
        ON signup_email_outbox(state, next_attempt_ms, locked_until_ms, id);

      CREATE TABLE IF NOT EXISTS oauth_attempts (
        id TEXT PRIMARY KEY,
        browser_cookie_hash TEXT NOT NULL CHECK(length(browser_cookie_hash) = 64 AND browser_cookie_hash NOT GLOB '*[^0-9a-f]*'),
        oauth_cookie_hash TEXT NOT NULL UNIQUE CHECK(length(oauth_cookie_hash) = 64 AND oauth_cookie_hash NOT GLOB '*[^0-9a-f]*'),
        state_hash TEXT NOT NULL UNIQUE CHECK(length(state_hash) = 64 AND state_hash NOT GLOB '*[^0-9a-f]*'),
        pkce_verifier_encrypted TEXT NOT NULL CHECK(length(CAST(pkce_verifier_encrypted AS BLOB)) BETWEEN 1 AND ${MAX_ENCRYPTED_VALUE_BYTES}),
        nonce TEXT NOT NULL CHECK(length(CAST(nonce AS BLOB)) BETWEEN 22 AND 256),
        intent TEXT NOT NULL CHECK(intent IN ('login','signup','link')),
        route_key TEXT CHECK(route_key IS NULL OR (length(route_key) = 64 AND route_key NOT GLOB '*[^0-9a-f]*')),
        link_ticket_encrypted TEXT CHECK(link_ticket_encrypted IS NULL OR length(CAST(link_ticket_encrypted AS BLOB)) BETWEEN 1 AND ${MAX_ENCRYPTED_VALUE_BYTES}),
        proof_at_ms INTEGER,
        created_at_ms INTEGER NOT NULL CHECK(created_at_ms >= 0),
        expires_at_ms INTEGER NOT NULL CHECK(expires_at_ms > created_at_ms AND expires_at_ms - created_at_ms = ${OAUTH_ATTEMPT_TTL_MS}),
        consumed_at_ms INTEGER,
        invalidated_at_ms INTEGER,
        CHECK(consumed_at_ms IS NULL OR consumed_at_ms BETWEEN created_at_ms AND expires_at_ms),
        CHECK(invalidated_at_ms IS NULL OR invalidated_at_ms >= created_at_ms),
        CHECK(consumed_at_ms IS NULL OR invalidated_at_ms IS NULL),
        CHECK((intent = 'link' AND route_key IS NOT NULL AND link_ticket_encrypted IS NOT NULL AND proof_at_ms IS NULL)
          OR (intent = 'signup' AND route_key IS NULL AND link_ticket_encrypted IS NULL AND proof_at_ms IS NOT NULL)
          OR (intent = 'login' AND route_key IS NULL AND link_ticket_encrypted IS NULL AND proof_at_ms IS NULL)),
        CHECK(proof_at_ms IS NULL OR proof_at_ms BETWEEN created_at_ms - 300000 AND created_at_ms)
      );
      CREATE UNIQUE INDEX IF NOT EXISTS oauth_attempts_one_live_browser
        ON oauth_attempts(browser_cookie_hash) WHERE consumed_at_ms IS NULL AND invalidated_at_ms IS NULL;
      CREATE INDEX IF NOT EXISTS oauth_attempts_expiry ON oauth_attempts(expires_at_ms);

      CREATE TABLE IF NOT EXISTS result_receipts (
        id TEXT PRIMARY KEY,
        receipt_hash TEXT NOT NULL UNIQUE CHECK(length(receipt_hash) = 64 AND receipt_hash NOT GLOB '*[^0-9a-f]*'),
        browser_cookie_hash TEXT NOT NULL CHECK(length(browser_cookie_hash) = 64 AND browser_cookie_hash NOT GLOB '*[^0-9a-f]*'),
        job_id TEXT NOT NULL UNIQUE CHECK(length(CAST(job_id AS BLOB)) BETWEEN 1 AND 256),
        state TEXT NOT NULL CHECK(state IN ('pending','awaiting-device','ready','proof-required','capacity','failed')),
        route_key TEXT CHECK(route_key IS NULL OR (length(route_key) = 64 AND route_key NOT GLOB '*[^0-9a-f]*')),
        assertion_input_encrypted TEXT CHECK(assertion_input_encrypted IS NULL OR length(CAST(assertion_input_encrypted AS BLOB)) BETWEEN 1 AND ${MAX_ENCRYPTED_VALUE_BYTES}),
        assertion_encrypted TEXT CHECK(assertion_encrypted IS NULL OR length(CAST(assertion_encrypted AS BLOB)) BETWEEN 1 AND ${MAX_ENCRYPTED_VALUE_BYTES}),
        bound_fingerprint TEXT CHECK(bound_fingerprint IS NULL OR (length(bound_fingerprint) = 64 AND bound_fingerprint NOT GLOB '*[^0-9a-f]*')),
        created_at_ms INTEGER NOT NULL CHECK(created_at_ms >= 0),
        expires_at_ms INTEGER NOT NULL CHECK(expires_at_ms > created_at_ms AND expires_at_ms - created_at_ms <= ${MAX_RESULT_TTL_MS}),
        updated_at_ms INTEGER NOT NULL CHECK(updated_at_ms >= created_at_ms),
        invalidated_at_ms INTEGER,
        CHECK((state = 'pending' AND route_key IS NULL AND assertion_input_encrypted IS NULL AND assertion_encrypted IS NULL AND bound_fingerprint IS NULL)
          OR (state = 'awaiting-device' AND route_key IS NOT NULL AND assertion_input_encrypted IS NOT NULL AND assertion_encrypted IS NULL AND bound_fingerprint IS NULL)
          OR (state = 'ready' AND route_key IS NOT NULL AND assertion_input_encrypted IS NOT NULL AND assertion_encrypted IS NOT NULL AND bound_fingerprint IS NOT NULL)
          OR (state IN ('proof-required','capacity','failed') AND route_key IS NULL AND assertion_input_encrypted IS NULL AND assertion_encrypted IS NULL AND bound_fingerprint IS NULL))
      );
      CREATE INDEX IF NOT EXISTS result_receipts_browser_live
        ON result_receipts(browser_cookie_hash, invalidated_at_ms, expires_at_ms);
      PRAGMA user_version=1;
    `);
  });
}

export function openGatewayStateContext(options: OpenGatewayStateStoreOptions): GatewayStateContext {
  const path = resolve(options.path ?? DEFAULT_STATE_PATH);
  const now = options.now ?? Date.now;
  const createId = options.createId ?? randomUUID;
  const cipher = new GatewayStateCipher(options.oauthStateKey);
  const parent = dirname(path);
  mkdirSync(parent, { recursive: true, mode: 0o700 });
  chmodSync(parent, 0o700);
  const sqlite = new Database(path, { create: true });
  try {
    chmodSync(path, 0o600);
    initializeGatewayStateDatabase(sqlite);
  } catch (error) {
    sqlite.close();
    throw error;
  }
  return {
    path,
    sqlite,
    now,
    createId,
    cipher,
  };
}
