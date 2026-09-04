// This module owns shared managed-credential checks and pending topology validation.
// The managed-container invariant inspector calls it before any active topology checks.
// It depends on SQLite and native-email normalization at the persistence boundary.
// Query and assertion order must stay stable because the first invariant failure is observable.
import type { Database } from "bun:sqlite";
import { normalizeAccountEmail } from "@roost/shared/native-credentials";

export const CANONICAL_UUID_RE =
  /^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;
export const SHA256_HEX_RE = /^[0-9a-f]{64}$/;
export const MAX_PASSWORD_RESET_HISTORY_ROWS = 1_024;
export const MAX_EMAIL_OUTBOX_HISTORY_ROWS = MAX_PASSWORD_RESET_HISTORY_ROWS + 1;

/** Every surviving tenant-runtime table introduced or rebuilt by migration 0021. */
export const MANAGED_RUNTIME_SCOPE_TABLES = [
  "workers", "bootstrap_tokens", "events", "sessions", "workspaces",
  "workspace_sessions", "tasks", "mcp_relays", "audit_log", "app_settings",
  "push_subscriptions",
] as const;

const PENDING_EMPTY_TABLES = [
  "account_identities", "account_devices", "organizations",
  "organization_memberships", "dashboards", "dashboard_memberships",
  "authorized_keys", "authorized_key_revocations", "pair_requests",
  "password_reset_tokens", "federated_assertion_redemptions",
] as const;

export type ManagedCredentialTopology =
  | "pending-coordinator-email"
  | "pending-signup-gateway"
  | "active-native-password"
  | "active-passwordless-google"
  | "active-linked";

export interface ActivationRow {
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
}

export interface AccountRow {
  id: string;
  email_normalized: string;
  password_hash: string | null;
  status: string;
  created_at_ms: number;
  password_changed_at_ms: number | null;
}

type SQLiteBinding = string | number | bigint | boolean | Uint8Array | null;

export function fail(detail: string): never {
  throw new Error(`managed-container invariant violation: ${detail}`);
}

export function isTimestamp(value: number | null): value is number {
  return value !== null && Number.isSafeInteger(value) && value >= 0;
}

export function queryGet<Row>(
  sqlite: Database,
  sql: string,
  ...bindings: SQLiteBinding[]
): Row | null {
  const statement = sqlite.prepare(sql);
  try {
    return statement.get(...bindings) as Row | null;
  } finally {
    statement.finalize();
  }
}

export function queryAll<Row>(
  sqlite: Database,
  sql: string,
  ...bindings: SQLiteBinding[]
): Row[] {
  const statement = sqlite.prepare(sql);
  try {
    return statement.all(...bindings) as Row[];
  } finally {
    statement.finalize();
  }
}

function countRows(sqlite: Database, table: string): number {
  const row = queryGet<{ count: number }>(sqlite, `SELECT COUNT(*) AS count FROM ${table}`, );
  if (!row || !Number.isSafeInteger(row.count) || row.count < 0) fail(`could not count ${table}`);
  return row.count;
}

export function requireExactlyOne<T>(rows: T[], description: string): T {
  if (rows.length !== 1) fail(`expected exactly one ${description}`);
  return rows[0]!;
}

export function normalizedEmail(value: string): boolean {
  return value.length > 0 && normalizeAccountEmail(value) === value;
}

export function validateActivationIdentity(activation: ActivationRow, instanceId: string): void {
  if (activation.coordinator_id !== instanceId) fail("owner activation belongs to another coordinator");
  if (!CANONICAL_UUID_RE.test(activation.account_id)) fail("owner activation account ID is not canonical");
  if (!normalizedEmail(activation.email_normalized)) fail("owner activation email is not normalized");
  if (!SHA256_HEX_RE.test(activation.token_hash)) fail("owner activation token hash is not canonical");
  if (!isTimestamp(activation.created_at_ms)
    || !isTimestamp(activation.expires_at_ms)
    || activation.expires_at_ms <= activation.created_at_ms) {
    fail("owner activation timestamps are invalid");
  }
}

export function activationOutboxes(
  sqlite: Database,
): Array<{ id: string; kind: string; recipient: string }> {
  return queryAll<{ id: string; kind: string; recipient: string }>(sqlite, `
    SELECT id, kind, recipient FROM email_outbox
    LIMIT ${MAX_EMAIL_OUTBOX_HISTORY_ROWS + 1}
  `, );
}

function validatePendingDelivery(
  sqlite: Database,
  activation: ActivationRow,
): ManagedCredentialTopology {
  const outboxes = activationOutboxes(sqlite);
  if (activation.delivery === "coordinator-email") {
    if (activation.outbox_id === null) fail("coordinator-email activation has no outbox");
    const outbox = requireExactlyOne(outboxes, "owner activation outbox");
    if (outbox.id !== activation.outbox_id || outbox.kind !== "owner_activation"
      || outbox.recipient !== activation.email_normalized) {
      fail("owner activation outbox does not match the activation");
    }
    return "pending-coordinator-email";
  }
  if (activation.delivery === "signup-gateway") {
    if (activation.outbox_id !== null || outboxes.length !== 0) {
      fail("signup-gateway activation must not have coordinator email");
    }
    return "pending-signup-gateway";
  }
  fail("owner activation delivery is invalid");
}

export function assertPendingState(
  sqlite: Database,
  activation: ActivationRow,
  instanceId: string,
  now: number,
): ManagedCredentialTopology {
  validateActivationIdentity(activation, instanceId);
  if (activation.accepted_at_ms !== null) fail("pending owner activation was already accepted");
  if (activation.revoked_at_ms !== null) fail("pending owner activation was revoked");
  if (activation.expires_at_ms <= now) fail("pending owner activation expired");
  const topology = validatePendingDelivery(sqlite, activation);
  for (const table of PENDING_EMPTY_TABLES) {
    if (countRows(sqlite, table) !== 0) fail(`pending coordinator contains ${table} rows`);
  }
  for (const table of MANAGED_RUNTIME_SCOPE_TABLES) {
    if (table === "audit_log") {
      if (queryGet<{ present: number }>(sqlite, "SELECT 1 AS present FROM audit_log WHERE dashboard_id IS NOT NULL LIMIT 1", )) fail("pending coordinator contains scoped audit_log rows");
      continue;
    }
    if (table === "app_settings") {
      if (queryGet<{ present: number }>(sqlite, `
        SELECT 1 AS present FROM app_settings
        WHERE dashboard_id IS NOT NULL OR key <> 'push.vapid' LIMIT 1
      `, )) fail("pending coordinator contains tenant app_settings rows");
      continue;
    }
    if (countRows(sqlite, table) !== 0) fail(`pending coordinator contains ${table} rows`);
  }
  return topology;
}
