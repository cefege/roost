// Owns the single self-hosted account/organization/dashboard topology.
// Startup calls it around the tenancy migration so nullable legacy rows gain
// one exact authority before database write guards become mandatory.
import { randomUUID } from "node:crypto";
import type { Database } from "bun:sqlite";

export interface SelfHostedTenant {
  accountId: string;
  organizationId: string;
  dashboardId: string;
}

export interface EnsureSelfHostedTenantOptions {
  backfillLegacyScopes: boolean;
}

/** Runtime scope owned by the self-hosted cutover. Retired webhook and
 * permission tables are deliberately absent. */
const STRICT_RUNTIME_SCOPE_TABLES = [
  "workers",
  "bootstrap_tokens",
  "events",
  "sessions",
  "workspaces",
  "workspace_sessions",
  "tasks",
  "mcp_relays",
  "push_subscriptions",
] as const;

type SQLiteBinding = string | number | bigint | boolean | Uint8Array | null;

class SelfHostedTenantInvariantError extends Error {}

function fail(reason: string): never {
  throw new SelfHostedTenantInvariantError(
    `self-hosted tenant invariant violation: ${reason}`,
  );
}

function queryAll<Row>(
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

function queryGet<Row>(
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

function queryRun(
  sqlite: Database,
  sql: string,
  ...bindings: SQLiteBinding[]
): void {
  const statement = sqlite.prepare(sql);
  try {
    statement.run(...bindings);
  } finally {
    statement.finalize();
  }
}


function inspectOrCreateTopology(
  sqlite: Database,
  now: number,
): SelfHostedTenant {
  const accounts = queryAll<{ id: string; status: string }>(
    sqlite,
    "SELECT id, status FROM accounts LIMIT 2",
  );
  const organizations = queryAll<{ id: string; status: string }>(
    sqlite,
    "SELECT id, status FROM organizations LIMIT 2",
  );
  const dashboards = queryAll<{
    id: string;
    organization_id: string;
    status: string;
  }>(sqlite, "SELECT id, organization_id, status FROM dashboards LIMIT 2");
  const organizationMemberships = queryAll<{
    organization_id: string;
    account_id: string;
    role: string;
  }>(
    sqlite,
    "SELECT organization_id, account_id, role FROM organization_memberships LIMIT 2",
  );
  const dashboardMemberships = queryAll<{
    dashboard_id: string;
    account_id: string;
    role: string;
  }>(
    sqlite,
    "SELECT dashboard_id, account_id, role FROM dashboard_memberships LIMIT 2",
  );

  if (accounts.length > 1) fail("multiple accounts");
  if (organizations.length > 1) fail("multiple organizations");
  if (dashboards.length > 1) fail("multiple dashboards");

  if (accounts.length === 0) {
    if (
      organizations.length !== 0
      || dashboards.length !== 0
      || organizationMemberships.length !== 0
      || dashboardMemberships.length !== 0
      || queryGet(
        sqlite,
        "SELECT 1 AS present FROM account_identities LIMIT 1",
      ) !== null
      || queryGet(
        sqlite,
        "SELECT 1 AS present FROM account_devices LIMIT 1",
      ) !== null
    ) {
      fail("identity topology is partial");
    }

    const accountId = randomUUID();
    const organizationId = randomUUID();
    const dashboardId = randomUUID();
    queryRun(sqlite, `
      INSERT INTO accounts
        (id, email_normalized, password_hash, status, created_at_ms, password_changed_at_ms)
      VALUES (?, 'local@roost.invalid', NULL, 'active', ?, NULL)
    `, accountId, now);
    queryRun(sqlite, `
      INSERT INTO organizations (id, slug, name, status, created_at_ms)
      VALUES (?, 'personal', 'Personal', 'active', ?)
    `, organizationId, now);
    queryRun(sqlite, `
      INSERT INTO organization_memberships
        (organization_id, account_id, role, created_at_ms)
      VALUES (?, ?, 'owner', ?)
    `, organizationId, accountId, now);
    queryRun(sqlite, `
      INSERT INTO dashboards
        (id, organization_id, slug, name, status, created_at_ms)
      VALUES (?, ?, 'default', 'Personal', 'active', ?)
    `, dashboardId, organizationId, now);
    queryRun(sqlite, `
      INSERT INTO dashboard_memberships
        (dashboard_id, account_id, role, created_at_ms)
      VALUES (?, ?, 'admin', ?)
    `, dashboardId, accountId, now);
    return { accountId, organizationId, dashboardId };
  }

  if (
    organizations.length !== 1
    || dashboards.length !== 1
    || organizationMemberships.length !== 1
    || dashboardMemberships.length !== 1
  ) {
    fail("identity topology is partial");
  }

  const account = accounts[0]!;
  const organization = organizations[0]!;
  const dashboard = dashboards[0]!;
  const organizationMembership = organizationMemberships[0]!;
  const dashboardMembership = dashboardMemberships[0]!;

  if (account.status !== "active") fail("account is inactive");
  if (organization.status !== "active") fail("organization is inactive");
  if (dashboard.status !== "active") fail("dashboard is inactive");
  if (dashboard.organization_id !== organization.id) {
    fail("dashboard belongs to another organization");
  }
  if (
    organizationMembership.organization_id !== organization.id
    || organizationMembership.account_id !== account.id
    || organizationMembership.role !== "owner"
  ) {
    fail("organization owner membership is incomplete");
  }
  if (
    dashboardMembership.dashboard_id !== dashboard.id
    || dashboardMembership.account_id !== account.id
    || dashboardMembership.role !== "admin"
  ) {
    fail("dashboard admin membership is incomplete");
  }

  return {
    accountId: account.id,
    organizationId: organization.id,
    dashboardId: dashboard.id,
  };
}

function associateAuthorizedKeys(
  sqlite: Database,
  tenant: SelfHostedTenant,
  now: number,
): void {
  if (queryGet(sqlite, `
    SELECT 1 AS present
    FROM workers AS worker
    INNER JOIN account_devices AS device ON device.fingerprint = worker.fp
    LIMIT 1
  `)) {
    fail("authorized key is both a worker and account device");
  }
  if (queryGet(sqlite, `
    SELECT 1 AS present
    FROM account_devices AS device
    WHERE device.account_id <> ?
    LIMIT 1
  `, tenant.accountId)) {
    fail("account device belongs to another account");
  }
  if (queryGet(sqlite, `
    SELECT 1 AS present
    FROM account_devices AS device
    LEFT JOIN authorized_keys AS key ON key.fingerprint = device.fingerprint
    WHERE key.fingerprint IS NULL
    LIMIT 1
  `)) {
    fail("account device has no authorized key");
  }

  queryRun(sqlite, `
    INSERT INTO account_devices
      (fingerprint, account_id, added_at_ms, last_seen_at_ms)
    SELECT key.fingerprint, ?, ?, ?
    FROM authorized_keys AS key
    LEFT JOIN workers AS worker ON worker.fp = key.fingerprint
    LEFT JOIN account_devices AS device ON device.fingerprint = key.fingerprint
    WHERE worker.fp IS NULL AND device.fingerprint IS NULL
  `, tenant.accountId, now, now);

  if (queryGet(sqlite, `
    SELECT 1 AS present
    FROM authorized_keys AS key
    LEFT JOIN workers AS worker ON worker.fp = key.fingerprint
    LEFT JOIN account_devices AS device ON device.fingerprint = key.fingerprint
    WHERE worker.fp IS NULL
      AND (device.fingerprint IS NULL OR device.account_id <> ?)
    LIMIT 1
  `, tenant.accountId)) {
    fail("authorized browser key is not associated with the sole account");
  }
}

function validateAndBackfillRuntimeScope(
  sqlite: Database,
  tenant: SelfHostedTenant,
  backfillLegacyScopes: boolean,
): void {
  for (const table of STRICT_RUNTIME_SCOPE_TABLES) {
    if (queryGet(sqlite, `
      SELECT 1 AS present FROM ${table}
      WHERE dashboard_id IS NOT NULL AND dashboard_id <> ?
      LIMIT 1
    `, tenant.dashboardId)) {
      fail(`${table} contains foreign dashboard scope`);
    }
  }
  const bootstrapTokensHaveAccountScope = queryGet<{ present: number }>(
    sqlite,
    `SELECT 1 AS present
     FROM pragma_table_info('bootstrap_tokens')
     WHERE name = 'account_id'
     LIMIT 1`,
  ) !== null;
  if (bootstrapTokensHaveAccountScope && queryGet(sqlite, `
    SELECT 1 AS present FROM bootstrap_tokens
    WHERE account_id IS NULL OR account_id <> ?
    LIMIT 1
  `, tenant.accountId)) {
    fail("bootstrap_tokens contains missing or foreign account scope");
  }
  if (queryGet(sqlite, `
    SELECT 1 AS present FROM audit_log
    WHERE dashboard_id IS NOT NULL AND dashboard_id <> ?
    LIMIT 1
  `, tenant.dashboardId)) {
    fail("audit_log contains foreign dashboard scope");
  }
  if (queryGet(sqlite, `
    SELECT 1 AS present FROM app_settings
    WHERE (key = 'push.vapid' AND dashboard_id IS NOT NULL)
       OR (key <> 'push.vapid' AND dashboard_id IS NOT NULL AND dashboard_id <> ?)
    LIMIT 1
  `, tenant.dashboardId)) {
    fail("app_settings contains invalid dashboard scope");
  }

  if (backfillLegacyScopes) {
    for (const table of STRICT_RUNTIME_SCOPE_TABLES) {
      queryRun(
        sqlite,
        `UPDATE ${table} SET dashboard_id = ? WHERE dashboard_id IS NULL`,
        tenant.dashboardId,
      );
    }
    queryRun(
      sqlite,
      "UPDATE audit_log SET dashboard_id = ? WHERE dashboard_id IS NULL",
      tenant.dashboardId,
    );
    queryRun(sqlite, `
      UPDATE app_settings SET dashboard_id = ?
      WHERE dashboard_id IS NULL AND key <> 'push.vapid'
    `, tenant.dashboardId);
  }

  for (const table of STRICT_RUNTIME_SCOPE_TABLES) {
    if (queryGet(sqlite, `
      SELECT 1 AS present FROM ${table}
      WHERE dashboard_id IS NULL OR dashboard_id <> ?
      LIMIT 1
    `, tenant.dashboardId)) {
      fail(`${table} contains missing or foreign dashboard scope`);
    }
  }
  if (queryGet(sqlite, `
    SELECT 1 AS present FROM app_settings
    WHERE (key = 'push.vapid' AND dashboard_id IS NOT NULL)
       OR (key <> 'push.vapid' AND (dashboard_id IS NULL OR dashboard_id <> ?))
    LIMIT 1
  `, tenant.dashboardId)) {
    fail("app_settings contains missing or foreign dashboard scope");
  }
}

/**
 * Create or validate the single self-hosted tenant. All inspection, creation,
 * credential association, and optional legacy scoping share one write lock and
 * transaction so startup can never observe or leave a partial cutover.
 */
export function ensureSelfHostedTenant(
  sqlite: Database,
  options: EnsureSelfHostedTenantOptions,
): SelfHostedTenant {
  let transactionOpen = false;
  try {
    sqlite.exec("BEGIN IMMEDIATE");
    transactionOpen = true;
    const now = Date.now();
    const tenant = inspectOrCreateTopology(sqlite, now);
    associateAuthorizedKeys(sqlite, tenant, now);
    validateAndBackfillRuntimeScope(
      sqlite,
      tenant,
      options.backfillLegacyScopes,
    );
    sqlite.exec("COMMIT");
    transactionOpen = false;
    return tenant;
  } catch (error) {
    if (transactionOpen) {
      try {
        sqlite.exec("ROLLBACK");
      } catch {
        // Preserve the bounded initializer failure.
      }
    }
    if (error instanceof SelfHostedTenantInvariantError) throw error;
    fail("database operation failed");
  }
}
