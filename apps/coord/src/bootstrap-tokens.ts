// Single owner for one-shot worker/browser enrollment grants.
// Plaintext bearers remain process-local while SQLite stores only scoped
// SHA-256 digests and transactional claim state.
import { Database } from "bun:sqlite";
import { sql } from "kysely";
import type { KyselyDB } from "./db/connection.ts";
import { ensureSelfHostedTenant } from "./self-hosted-tenant.ts";

export type BootstrapTokenKind = "worker" | "browser";

export interface MintBootstrapTokenInput {
  kind: BootstrapTokenKind;
  label: string;
  accountId: string;
  dashboardId: string;
  mintedByFp: string | null;
  now?: number;
}

export interface MintHostBootstrapTokenInput {
  kind: BootstrapTokenKind;
  label: string;
  now?: number;
}

export interface MintedBootstrapToken {
  token: string;
  expiresAtMs: number;
}

export interface BootstrapTokenClaim {
  accountId: string;
  dashboardId: string;
  label: string;
  mintedByFp: string | null;
}

export interface ClaimBootstrapTokenInput {
  tokenHash: string;
  kind: BootstrapTokenKind;
  fingerprint: string;
  publicKey: Uint8Array;
  now: number;
}

const BOOTSTRAP_TOKEN_TTL_MS = 24 * 60 * 60 * 1_000;
const BOOTSTRAP_TOKEN_RANDOM_BYTES = 24;

function randomBootstrapBearer(): string {
  const random = new Uint8Array(BOOTSTRAP_TOKEN_RANDOM_BYTES);
  crypto.getRandomValues(random);
  return `roost_bt_${Buffer.from(random).toString("hex")}`;
}

export async function bootstrapTokenDigest(plaintext: string): Promise<string> {
  const bytes = new TextEncoder().encode(plaintext);
  const digest = await crypto.subtle.digest("SHA-256", bytes);
  return Buffer.from(digest).toString("hex");
}

async function makeBootstrapTokenRow(input: MintBootstrapTokenInput): Promise<{
  token: string;
  tokenHash: string;
  createdAtMs: number;
  expiresAtMs: number;
}> {
  const token = randomBootstrapBearer();
  const tokenHash = await bootstrapTokenDigest(token);
  const createdAtMs = input.now ?? Date.now();
  return {
    token,
    tokenHash,
    createdAtMs,
    expiresAtMs: createdAtMs + BOOTSTRAP_TOKEN_TTL_MS,
  };
}

/** Mint a one-shot grant. Only the returned value contains the live bearer. */
export async function mintBootstrapToken(
  db: KyselyDB,
  input: MintBootstrapTokenInput,
): Promise<MintedBootstrapToken> {
  const row = await makeBootstrapTokenRow(input);
  await db.insertInto("bootstrap_tokens").values({
    token_hash: row.tokenHash,
    account_id: input.accountId,
    dashboard_id: input.dashboardId,
    kind: input.kind,
    label: input.label,
    created_at_ms: row.createdAtMs,
    expires_at_ms: row.expiresAtMs,
    used_at_ms: null,
    used_by_fp: null,
    minted_by_fp: input.mintedByFp,
  }).execute();
  return { token: row.token, expiresAtMs: row.expiresAtMs };
}

/**
 * Host-local quickstart minting. The topology is validated before the scoped
 * digest row is written; the plaintext bearer never enters SQLite.
 */
export async function mintHostBootstrapToken(
  databasePath: string,
  input: MintHostBootstrapTokenInput,
): Promise<MintedBootstrapToken> {
  const sqlite = new Database(databasePath, { strict: true });
  try {
    sqlite.exec("PRAGMA foreign_keys=ON");
    sqlite.exec("PRAGMA busy_timeout=5000");
    const tenant = ensureSelfHostedTenant(sqlite, { backfillLegacyScopes: false });
    const row = await makeBootstrapTokenRow({
      ...input,
      accountId: tenant.accountId,
      dashboardId: tenant.dashboardId,
      mintedByFp: null,
    });
    sqlite.query(`
      INSERT INTO bootstrap_tokens (
        token_hash, account_id, dashboard_id, kind, label,
        created_at_ms, expires_at_ms, used_at_ms, used_by_fp, minted_by_fp
      ) VALUES (?, ?, ?, ?, ?, ?, ?, NULL, NULL, NULL)
    `).run(
      row.tokenHash,
      tenant.accountId,
      tenant.dashboardId,
      input.kind,
      input.label,
      row.createdAtMs,
      row.expiresAtMs,
    );
    return { token: row.token, expiresAtMs: row.expiresAtMs };
  } finally {
    sqlite.close(true);
  }
}

/**
 * Atomically claims an unused grant, or recognizes an exact same-key retry
 * only when its already-created principal remains consistent with the grant.
 * The caller must keep this UPDATE in the transaction that creates/validates
 * that principal.
 */
export async function claimBootstrapToken(
  db: KyselyDB,
  input: ClaimBootstrapTokenInput,
): Promise<BootstrapTokenClaim | null> {
  const result = await sql<{
    accountId: string;
    dashboardId: string;
    label: string;
    mintedByFp: string | null;
  }>`
    UPDATE bootstrap_tokens AS bt
    SET used_at_ms = CASE WHEN bt.used_at_ms IS NULL THEN ${input.now} ELSE bt.used_at_ms END,
        used_by_fp = CASE WHEN bt.used_at_ms IS NULL THEN ${input.fingerprint} ELSE bt.used_by_fp END
    WHERE bt.token_hash = ${input.tokenHash}
      AND bt.kind = ${input.kind}
      AND bt.expires_at_ms >= ${input.now}
      AND NOT EXISTS (
        SELECT 1
        FROM authorized_key_revocations AS submitted_revocation
        WHERE submitted_revocation.fingerprint = ${input.fingerprint}
      )
      AND EXISTS (
        SELECT 1
        FROM accounts AS account
        JOIN dashboard_memberships AS dashboard_membership
          ON dashboard_membership.account_id = account.id
         AND dashboard_membership.dashboard_id = bt.dashboard_id
        JOIN dashboards AS dashboard
          ON dashboard.id = dashboard_membership.dashboard_id
        JOIN organizations AS organization
          ON organization.id = dashboard.organization_id
        JOIN organization_memberships AS organization_membership
          ON organization_membership.account_id = account.id
         AND organization_membership.organization_id = organization.id
        WHERE account.id = bt.account_id
          AND account.status = 'active'
          AND dashboard.status = 'active'
          AND organization.status = 'active'
      )
      AND (
        bt.minted_by_fp IS NULL
        OR EXISTS (
          SELECT 1
          FROM authorized_keys AS minter_key
          JOIN account_devices AS minter_device
            ON minter_device.fingerprint = minter_key.fingerprint
           AND minter_device.account_id = bt.account_id
          WHERE minter_key.fingerprint = bt.minted_by_fp
            AND NOT EXISTS (
              SELECT 1
              FROM authorized_key_revocations AS minter_revocation
              WHERE minter_revocation.fingerprint = minter_key.fingerprint
            )
        )
      )
      AND (
        (
          bt.used_at_ms IS NULL
          AND NOT EXISTS (
            SELECT 1 FROM authorized_keys AS existing_key
            WHERE existing_key.fingerprint = ${input.fingerprint}
          )
          AND NOT EXISTS (
            SELECT 1 FROM workers AS existing_worker
            WHERE existing_worker.fp = ${input.fingerprint}
          )
          AND NOT EXISTS (
            SELECT 1 FROM account_devices AS existing_device
            WHERE existing_device.fingerprint = ${input.fingerprint}
          )
        )
        OR (
          bt.used_at_ms IS NOT NULL
          AND bt.used_by_fp = ${input.fingerprint}
          AND EXISTS (
            SELECT 1
            FROM authorized_keys AS retry_key
            WHERE retry_key.fingerprint = ${input.fingerprint}
              AND retry_key.public_key = ${input.publicKey}
          )
          AND (
            (${input.kind} = 'worker' AND EXISTS (
              SELECT 1
              FROM workers AS retry_worker
              WHERE retry_worker.fp = ${input.fingerprint}
                AND retry_worker.dashboard_id = bt.dashboard_id
                AND retry_worker.deleted_at_ms IS NULL
            ))
            OR (${input.kind} = 'browser' AND EXISTS (
              SELECT 1
              FROM account_devices AS retry_device
              WHERE retry_device.fingerprint = ${input.fingerprint}
                AND retry_device.account_id = bt.account_id
            ))
          )
        )
      )
    RETURNING account_id AS accountId,
              dashboard_id AS dashboardId,
              label,
              minted_by_fp AS mintedByFp
  `.execute(db);
  return result.rows[0] ?? null;
}
