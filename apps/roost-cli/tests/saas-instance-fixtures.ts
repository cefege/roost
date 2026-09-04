/**
 * Shared fixtures keep SaaS instance databases, constants, and row readers consistent.
 * Sibling suites own their temporary-root lists so cleanup cannot cross test files.
 * Centralized decryption assertions keep the split suites aligned on secret handling.
 */
import { Database } from "bun:sqlite";
import { expect } from "bun:test";
import { createHash } from "node:crypto";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { createEmailOutboxPayloadCipher } from "@roost/shared/email-payload";
import { runMigrations } from "../../coord/src/db/migrate.ts";

export const ROOT = resolve(import.meta.dir, "../../..");
export const ACCOUNT_ID = "11111111-1111-4111-8111-111111111111";
export const COORDINATOR_ID = "22222222-2222-4222-8222-222222222222";
export const EMAIL = "owner@example.com";
export const OUTBOX_KEY = Buffer.alloc(32, 7).toString("base64url");
export const PUBLIC_URL = "https://dashboard.roosttt.com";
export const ROUTE_KEY = "a".repeat(64);

interface ActivationRow {
  coordinator_id: string;
  account_id: string;
  email_normalized: string;
  token_hash: string;
  outbox_id: string;
  created_at_ms: number;
  expires_at_ms: number;
  accepted_at_ms: number | null;
  revoked_at_ms: number | null;
}

interface OutboxRow {
  id: string;
  kind: string;
  recipient: string;
  encrypted_payload: string;
  state: string;
  attempts: number;
  next_attempt_ms: number;
}

export async function createMigratedDatabase(
  roots: string[],
): Promise<{ root: string; path: string; sqlite: Database }> {
  const root = await mkdtemp(join(tmpdir(), "roost-saas-instance-"));
  roots.push(root);
  const path = join(root, "coordinator_v2.db");
  const sqlite = new Database(path, { create: true });
  sqlite.exec("PRAGMA foreign_keys=ON");
  await runMigrations(sqlite);
  return { root, path, sqlite };
}

export async function cleanupSaasInstanceRoots(roots: string[]): Promise<void> {
  while (roots.length > 0) {
    const root = roots.pop();
    if (root) await rm(root, { recursive: true, force: true });
  }
}

export function activation(sqlite: Database): ActivationRow {
  const row = sqlite.query<ActivationRow, []>(
    `SELECT coordinator_id, account_id, email_normalized, token_hash, outbox_id,
            created_at_ms, expires_at_ms, accepted_at_ms, revoked_at_ms
     FROM owner_activation_tokens`,
  ).get();
  if (!row) throw new Error("missing activation fixture row");
  return row;
}

export function outbox(sqlite: Database): OutboxRow {
  const row = sqlite.query<OutboxRow, []>(
    `SELECT id, kind, recipient, encrypted_payload, state, attempts, next_attempt_ms
     FROM email_outbox WHERE kind = 'owner_activation'`,
  ).get();
  if (!row) throw new Error("missing activation outbox fixture row");
  return row;
}

export function rowCount(sqlite: Database, table: string): number {
  const row = sqlite.query<{ count: number }, []>(`SELECT COUNT(*) AS count FROM ${table}`).get();
  if (!row) throw new Error(`missing count for ${table}`);
  return row.count;
}

export function decryptedToken(row: OutboxRow): { link: string; token: string } {
  const payload = createEmailOutboxPayloadCipher(OUTBOX_KEY).decrypt(
    { outboxId: row.id, kind: row.kind },
    row.encrypted_payload,
  );
  const match = payload.text?.match(/(https:\/\/[^\s]+\/activate\/[0-9a-f]{64}#([A-Za-z0-9_-]+))$/);
  if (!match?.[1] || !match[2]) throw new Error("activation payload did not contain the fragment link");
  expect(payload.subject).toBe("Activate your Roost account");
  expect(payload.html).toContain(match[1]);
  return { link: match[1], token: match[2] };
}

export function hashToken(token: string): string {
  return createHash("sha256").update(token, "utf8").digest("hex");
}
