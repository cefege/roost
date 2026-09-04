// This module owns managed-container inspection and active credential topology checks.
// Coordinator startup calls it after migrations to reject unsafe tenant state.
// It depends on SQLite, route-key validation, and the pending-topology sibling.
// Validation order must stay stable because callers observe the first invariant failure.
import type { Database } from "bun:sqlite";
import type { CoordConfig } from "@roost/shared/config";
import { isTenantRouteKey } from "@roost/shared/tenant-route";
import {
  activationOutboxes,
  type AccountRow,
  type ActivationRow,
  assertPendingState,
  CANONICAL_UUID_RE,
  fail,
  isTimestamp,
  MANAGED_RUNTIME_SCOPE_TABLES,
  MAX_EMAIL_OUTBOX_HISTORY_ROWS,
  MAX_PASSWORD_RESET_HISTORY_ROWS,
  type ManagedCredentialTopology,
  normalizedEmail,
  queryAll,
  queryGet,
  requireExactlyOne,
  SHA256_HEX_RE,
  validateActivationIdentity,
} from "./managed-credential-topology-validation.ts";

export { MANAGED_RUNTIME_SCOPE_TABLES };
export type { ManagedCredentialTopology };

const GOOGLE_ISSUER = "https://accounts.google.com";
const GOOGLE_SUBJECT_MAX_BYTES = 255;
const MAX_IDENTITY_HISTORY_ROWS = 64;

export interface ManagedContainerState {
  topology: ManagedCredentialTopology;
  activated: boolean;
  accountId: string;
  coordinatorId: string;
  expiresAtMs: number;
}

interface IdentityRow {
  account_id: string;
  issuer: string;
  subject: string;
  email_normalized: string;
  linked_at_ms: number;
  last_authenticated_at_ms: number | null;
  revoked_at_ms: number | null;
}

function validateIdentityHistory(
  sqlite: Database,
  account: AccountRow,
): { activeNative: boolean; activeGoogle: boolean } {
  const identities = queryAll<IdentityRow>(sqlite, `
    SELECT account_id, issuer, subject, email_normalized, linked_at_ms,
           last_authenticated_at_ms, revoked_at_ms
    FROM account_identities LIMIT ${MAX_IDENTITY_HISTORY_ROWS + 1}
  `, );
  if (identities.length === 0 || identities.length > MAX_IDENTITY_HISTORY_ROWS) {
    fail("account identity history is empty or exceeds its bound");
  }
  let nativeCount = 0;
  let activeNative = false;
  let activeGoogleCount = 0;
  for (const identity of identities) {
    if (identity.account_id !== account.id) fail("account identity belongs to another account");
    if (!normalizedEmail(identity.email_normalized)) fail("account identity email is not normalized");
    if (!isTimestamp(identity.linked_at_ms)
      || (identity.last_authenticated_at_ms !== null
        && (!isTimestamp(identity.last_authenticated_at_ms)
          || identity.last_authenticated_at_ms < identity.linked_at_ms))
      || (identity.revoked_at_ms !== null
        && (!isTimestamp(identity.revoked_at_ms) || identity.revoked_at_ms < identity.linked_at_ms))) {
      fail("account identity timestamps are invalid");
    }
    if (identity.issuer === "native") {
      nativeCount += 1;
      if (identity.subject !== account.id || identity.email_normalized !== account.email_normalized
        || nativeCount > 1) fail("native identity is not canonical");
      activeNative = identity.revoked_at_ms === null;
      continue;
    }
    if (identity.issuer !== GOOGLE_ISSUER) fail("account identity issuer is not supported");
    if (identity.subject.length === 0
      || Buffer.byteLength(identity.subject, "utf8") > GOOGLE_SUBJECT_MAX_BYTES
      || /[\u0000-\u001f\u007f-\u009f]/u.test(identity.subject)) {
      fail("Google identity subject is not canonical");
    }
    if (identity.revoked_at_ms === null) activeGoogleCount += 1;
  }
  if (activeGoogleCount > 1) fail("account has more than one active Google identity");
  return { activeNative, activeGoogle: activeGoogleCount === 1 };
}

function validateActiveActivation(
  activation: ActivationRow | undefined,
  account: AccountRow,
  instanceId: string,
  activeNative: boolean,
): void {
  if (!activation) return;
  validateActivationIdentity(activation, instanceId);
  if (activation.account_id !== account.id || activation.email_normalized !== account.email_normalized
    || !isTimestamp(activation.accepted_at_ms)
    || activation.accepted_at_ms < activation.created_at_ms
    || activation.accepted_at_ms > activation.expires_at_ms
    || activation.revoked_at_ms !== null || !activeNative) {
    fail("active account does not match an accepted owner activation");
  }
  if ((activation.delivery === "coordinator-email" && activation.outbox_id === null)
    || (activation.delivery === "signup-gateway" && activation.outbox_id !== null)
    || (activation.delivery !== "coordinator-email" && activation.delivery !== "signup-gateway")) {
    fail("active owner activation delivery is invalid");
  }
}
function validateResetAndOutboxHistory(
  sqlite: Database,
  account: AccountRow,
  activation: ActivationRow | undefined,
  activeNative: boolean,
): void {
  const outboxes = activationOutboxes(sqlite);
  if (outboxes.length > MAX_EMAIL_OUTBOX_HISTORY_ROWS) fail("email outbox history exceeds its bound");
  const ownerOutboxes = outboxes.filter((row) => row.kind === "owner_activation");
  const resetOutboxes = outboxes.filter((row) => row.kind === "password_reset");
  if (ownerOutboxes.length + resetOutboxes.length !== outboxes.length) {
    fail("email outbox contains an unsupported managed message");
  }
  if (resetOutboxes.length > MAX_PASSWORD_RESET_HISTORY_ROWS || (!activeNative && resetOutboxes.length > 0)) {
    fail("password-reset outbox history is invalid");
  }
  if (outboxes.some((row) => row.recipient !== account.email_normalized)) {
    fail("email outbox recipient does not match the account");
  }
  if (activation?.delivery === "coordinator-email") {
    if (ownerOutboxes.length !== 1 || ownerOutboxes[0]!.id !== activation.outbox_id) {
      fail("owner activation outbox does not match the activation");
    }
  } else if (ownerOutboxes.length !== 0) {
    fail("owner activation outbox exists for external or absent delivery");
  }
  const resets = queryAll<{
    account_id: string; email_normalized: string; token_hash: string;
    expires_at_ms: number; used_at_ms: number | null;
  }>(sqlite, `
    SELECT account_id, email_normalized, token_hash, expires_at_ms, used_at_ms
    FROM password_reset_tokens LIMIT ${MAX_PASSWORD_RESET_HISTORY_ROWS + 1}
  `, );
  if (resets.length > MAX_PASSWORD_RESET_HISTORY_ROWS || (!activeNative && resets.length > 0)) {
    fail("password-reset token history is invalid");
  }
  for (const reset of resets) {
    if (reset.account_id !== account.id || reset.email_normalized !== account.email_normalized
      || !SHA256_HEX_RE.test(reset.token_hash) || !isTimestamp(reset.expires_at_ms)
      || (reset.used_at_ms !== null && !isTimestamp(reset.used_at_ms))) {
      fail("password-reset token history is noncanonical or cross-account");
    }
  }
}

function assertActiveTopology(
  sqlite: Database,
  account: AccountRow,
  activation: ActivationRow | undefined,
  instanceId: string,
): ManagedCredentialTopology {
  if (!CANONICAL_UUID_RE.test(account.id) || !normalizedEmail(account.email_normalized)
    || account.status !== "active" || !isTimestamp(account.created_at_ms)) {
    fail("active account record is incomplete");
  }
  const { activeNative, activeGoogle } = validateIdentityHistory(sqlite, account);
  const hasPassword = typeof account.password_hash === "string" && account.password_hash.length > 0;
  if (hasPassword !== activeNative
    || (hasPassword && (!isTimestamp(account.password_changed_at_ms)
      || account.password_changed_at_ms < account.created_at_ms))
    || (!hasPassword && (account.password_hash !== null || account.password_changed_at_ms !== null))) {
    fail("password and active native identity are not equivalent");
  }
  if (!activeNative && !activeGoogle) fail("account has no active login identity");
  validateActiveActivation(activation, account, instanceId, activeNative);
  validateResetAndOutboxHistory(sqlite, account, activation, activeNative);

  const organization = requireExactlyOne(queryAll<{
    id: string; slug: string; name: string; status: string;
  }>(sqlite, "SELECT id, slug, name, status FROM organizations LIMIT 2", ), "organization");
  if (organization.id !== account.id || organization.slug !== "personal"
    || organization.name !== account.email_normalized || organization.status !== "active") {
    fail("organization is not the active personal account organization");
  }
  const organizationMembership = requireExactlyOne(queryAll<{
    organization_id: string; account_id: string; role: string;
  }>(sqlite, "SELECT organization_id, account_id, role FROM organization_memberships LIMIT 2", ),
  "organization membership");
  if (organizationMembership.organization_id !== account.id
    || organizationMembership.account_id !== account.id || organizationMembership.role !== "owner") {
    fail("organization membership is not the account owner membership");
  }
  const dashboard = requireExactlyOne(queryAll<{
    id: string; organization_id: string; slug: string; name: string; status: string;
  }>(sqlite, "SELECT id, organization_id, slug, name, status FROM dashboards LIMIT 2", ), "dashboard");
  if (dashboard.id !== instanceId || dashboard.organization_id !== account.id
    || dashboard.slug !== "default" || dashboard.name !== "Personal" || dashboard.status !== "active") {
    fail("dashboard is not the active instance dashboard");
  }
  const dashboardMembership = requireExactlyOne(queryAll<{
    dashboard_id: string; account_id: string; role: string;
  }>(sqlite, "SELECT dashboard_id, account_id, role FROM dashboard_memberships LIMIT 2", ),
  "dashboard membership");
  if (dashboardMembership.dashboard_id !== instanceId
    || dashboardMembership.account_id !== account.id || dashboardMembership.role !== "admin") {
    fail("dashboard membership is not the owner admin membership");
  }

  if (queryGet<{ present: number }>(sqlite, "SELECT 1 AS present FROM account_devices WHERE account_id <> ? LIMIT 1", account.id)) fail("account device belongs to another account");
  if (queryGet<{ present: number }>(sqlite, `
    SELECT 1 AS present FROM workers AS worker
    LEFT JOIN authorized_keys AS key ON key.fingerprint = worker.fp
    WHERE worker.deleted_at_ms IS NULL
      AND key.fingerprint IS NULL
    LIMIT 1
  `, )) fail("active worker has no matching authorized key");
  if (queryGet<{ present: number }>(sqlite, `
    SELECT 1 AS present FROM workers AS worker
    INNER JOIN authorized_key_revocations AS revocation
      ON revocation.fingerprint = worker.fp
    WHERE worker.deleted_at_ms IS NULL
    LIMIT 1
  `, )) fail("active worker has a matching revocation");
  if (queryGet<{ present: number }>(sqlite, `
    SELECT 1 AS present FROM workers AS worker
    INNER JOIN authorized_keys AS key ON key.fingerprint = worker.fp
    WHERE worker.deleted_at_ms IS NOT NULL
    LIMIT 1
  `, )) fail("tombstoned worker retains an authorized key");
  if (queryGet<{ present: number }>(sqlite, `
    SELECT 1 AS present FROM workers AS worker
    LEFT JOIN authorized_key_revocations AS revocation
      ON revocation.fingerprint = worker.fp
    WHERE worker.deleted_at_ms IS NOT NULL
      AND revocation.fingerprint IS NULL
    LIMIT 1
  `, )) fail("tombstoned worker has no matching revocation");
  if (queryGet<{ present: number }>(sqlite, `
    SELECT 1 AS present FROM authorized_keys AS key
    LEFT JOIN account_devices AS device ON device.fingerprint = key.fingerprint
    LEFT JOIN workers AS worker
      ON worker.fp = key.fingerprint
      AND worker.deleted_at_ms IS NULL
    WHERE (device.fingerprint IS NULL AND worker.fp IS NULL)
       OR (device.fingerprint IS NOT NULL AND worker.fp IS NOT NULL) LIMIT 1
  `, )) fail("authorized key is not owned by exactly one device or active worker");
  if (queryGet<{ present: number }>(sqlite, `
    SELECT 1 AS present FROM authorized_keys AS key
    INNER JOIN authorized_key_revocations AS revocation ON revocation.fingerprint = key.fingerprint
    LIMIT 1
  `, )) fail("revoked authorized key remains active");

  for (const table of MANAGED_RUNTIME_SCOPE_TABLES) {
    let foreignScope: { present: number } | null;
    if (table === "audit_log") {
      foreignScope = queryGet<{ present: number }>(sqlite, `
        SELECT 1 AS present FROM audit_log
        WHERE dashboard_id IS NOT NULL AND dashboard_id <> ? LIMIT 1
      `, instanceId);
    } else if (table === "app_settings") {
      foreignScope = queryGet<{ present: number }>(sqlite, `
        SELECT 1 AS present FROM app_settings
        WHERE (key = 'push.vapid' AND dashboard_id IS NOT NULL)
           OR (key <> 'push.vapid' AND (dashboard_id IS NULL OR dashboard_id <> ?)) LIMIT 1
      `, instanceId);
    } else {
      foreignScope = queryGet<{ present: number }>(sqlite, `
        SELECT 1 AS present FROM ${table}
        WHERE dashboard_id IS NULL OR dashboard_id <> ? LIMIT 1
      `, instanceId);
    }
    if (foreignScope) fail(`${table} contains a row outside the allowed dashboard scope`);
  }
  if (queryGet<{ present: number }>(sqlite, `
    SELECT 1 AS present FROM push_subscriptions AS subscription
    LEFT JOIN account_devices AS device ON device.fingerprint = subscription.viewer_fp
    WHERE device.fingerprint IS NULL LIMIT 1
  `, )) fail("push subscription does not belong to an account device");

  if (activeNative && activeGoogle) return "active-linked";
  if (activeNative) return "active-native-password";
  return "active-passwordless-google";
}

export function inspectManagedContainerState(
  sqlite: Database,
  instanceId: string,
  now = Date.now(),
): ManagedContainerState {
  if (!CANONICAL_UUID_RE.test(instanceId)) fail("managed profile has no canonical instance ID");
  if (!Number.isSafeInteger(now) || now < 0) fail("managed invariant clock is invalid");
    const accounts = queryAll<AccountRow>(sqlite, `
      SELECT id, email_normalized, password_hash, status, created_at_ms, password_changed_at_ms
      FROM accounts LIMIT 2
    `, );
    if (accounts.length > 1) fail("expected at most one account");
    const activations = queryAll<ActivationRow>(sqlite, `
      SELECT coordinator_id, account_id, email_normalized, token_hash, outbox_id,
             delivery, created_at_ms, expires_at_ms, accepted_at_ms, revoked_at_ms
      FROM owner_activation_tokens LIMIT 2
    `, );
    if (activations.length > 1) fail("expected at most one owner activation");
    if (accounts.length === 0) {
      const activation = requireExactlyOne(activations, "owner activation");
      const topology = assertPendingState(sqlite, activation, instanceId, now);
      return { topology, activated: false, accountId: activation.account_id,
        coordinatorId: activation.coordinator_id, expiresAtMs: activation.expires_at_ms };
    }
    const account = accounts[0]!;
    const activation = activations[0];
    const topology = assertActiveTopology(sqlite, account, activation, instanceId);
    return { topology, activated: true, accountId: account.id, coordinatorId: instanceId,
      expiresAtMs: activation?.expires_at_ms ?? Number.MAX_SAFE_INTEGER };
}

/** Abort startup unless the managed database has one explicit legal credential topology. */
export function assertManagedContainerInvariant(
  sqlite: Database,
  cfg: Pick<CoordConfig, "managedContainer" | "instanceId" | "tenantRouteKey">,
  now = Date.now(),
): void {
  if (!cfg.managedContainer) return;
  if (!cfg.instanceId || !CANONICAL_UUID_RE.test(cfg.instanceId)) {
    fail("managed profile has no canonical instance ID");
  }
  if (!isTenantRouteKey(cfg.tenantRouteKey)) fail("managed profile has no valid tenant route key");
  inspectManagedContainerState(sqlite, cfg.instanceId, now);
}
