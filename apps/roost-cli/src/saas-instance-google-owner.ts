/**
 * Seeds the first managed account from a verified Google identity assertion.
 * The hidden instance command calls this only after migrations and coordinator-key setup.
 * Exact topology checks make retries safe while rejecting every partial or divergent bootstrap.
 */

import type { Database } from "bun:sqlite";
import {
  canonicalSeedUuid,
  parseGoogleOwnerSeedPayload,
} from "./saas-instance-command.ts";
import {
  checkedOwnerActivationTime,
  countInstanceRows,
  EXACT_SEED_EMPTY_TABLES,
  requireEmptySeedTables,
  withInstanceTransaction,
} from "./saas-instance-seed-database.ts";
import type {
  OwnerActivationIdentity,
  SeedGoogleOwnerOptions,
  SeedGoogleOwnerPayload,
  SeedOwnerIdentityInput,
} from "./saas-instance-types.ts";

const GOOGLE_ISSUER = "https://accounts.google.com";

export function seedGoogleOwner(
  sqlite: Database,
  rawIdentity: SeedOwnerIdentityInput,
  payload: SeedGoogleOwnerPayload,
  options: SeedGoogleOwnerOptions = {},
): OwnerActivationIdentity {
  const accountId = canonicalSeedUuid(rawIdentity.accountId, "--account-id");
  const coordinatorId = canonicalSeedUuid(rawIdentity.coordinatorId, "--coordinator-id");
  const canonicalPayload = parseGoogleOwnerSeedPayload(
    new TextEncoder().encode(JSON.stringify(payload)),
  );
  const timestamp = checkedOwnerActivationTime(options.now ?? Date.now);
  return withInstanceTransaction(sqlite, "BEGIN IMMEDIATE", () => {
    const accounts = sqlite.query<{
      id: string;
      email_normalized: string;
      password_hash: string | null;
      status: string;
      created_at_ms: number;
      password_changed_at_ms: number | null;
    }, []>(`
      SELECT id, email_normalized, password_hash, status, created_at_ms,
             password_changed_at_ms
      FROM accounts
      LIMIT 2
    `).all();
    if (accounts.length === 1) {
      requireEmptySeedTables(sqlite, EXACT_SEED_EMPTY_TABLES);
      if (
        countInstanceRows(sqlite, "account_identities") !== 1
        || countInstanceRows(sqlite, "organizations") !== 1
        || countInstanceRows(sqlite, "organization_memberships") !== 1
        || countInstanceRows(sqlite, "dashboards") !== 1
        || countInstanceRows(sqlite, "dashboard_memberships") !== 1
      ) {
        throw new Error("refusing Google owner seed because topology differs");
      }
      const exact = sqlite.query<{ present: number }, [string, string, string, string, string]>(`
        SELECT 1 AS present
        FROM accounts AS account
        JOIN account_identities AS identity ON identity.account_id = account.id
        JOIN organizations AS organization ON organization.id = account.id
        JOIN organization_memberships AS organization_membership
          ON organization_membership.organization_id = organization.id
         AND organization_membership.account_id = account.id
        JOIN dashboards AS dashboard ON dashboard.organization_id = organization.id
        JOIN dashboard_memberships AS dashboard_membership
          ON dashboard_membership.dashboard_id = dashboard.id
         AND dashboard_membership.account_id = account.id
        WHERE account.id = ?
          AND account.email_normalized = ?
          AND account.password_hash IS NULL
          AND account.password_changed_at_ms IS NULL
          AND account.status = 'active'
          AND identity.issuer = ?
          AND identity.subject = ?
          AND identity.email_normalized = account.email_normalized
          AND identity.revoked_at_ms IS NULL
          AND organization.slug = 'personal'
          AND organization.name = account.email_normalized
          AND organization.status = 'active'
          AND organization_membership.role = 'owner'
          AND dashboard.id = ?
          AND dashboard.slug = 'default'
          AND dashboard.name = 'Personal'
          AND dashboard.status = 'active'
          AND dashboard_membership.role = 'admin'
      `).get(
        accountId,
        canonicalPayload.emailNormalized,
        GOOGLE_ISSUER,
        canonicalPayload.subject,
        coordinatorId,
      );
      if (!exact) throw new Error("refusing Google owner seed because topology differs");
      return { accountId, coordinatorId };
    }
    if (accounts.length !== 0) {
      throw new Error("refusing Google owner seed because account state is ambiguous");
    }
    requireEmptySeedTables(sqlite, [
      ...EXACT_SEED_EMPTY_TABLES,
      "account_identities",
      "organizations",
      "organization_memberships",
      "dashboards",
      "dashboard_memberships",
    ]);
    sqlite.query(`
      INSERT INTO accounts (
        id, email_normalized, password_hash, status, created_at_ms,
        password_changed_at_ms
      ) VALUES (?, ?, NULL, 'active', ?, NULL)
    `).run(accountId, canonicalPayload.emailNormalized, timestamp);
    sqlite.query(`
      INSERT INTO account_identities (
        account_id, issuer, subject, email_normalized, linked_at_ms,
        last_authenticated_at_ms, revoked_at_ms
      ) VALUES (?, ?, ?, ?, ?, ?, NULL)
    `).run(
      accountId,
      GOOGLE_ISSUER,
      canonicalPayload.subject,
      canonicalPayload.emailNormalized,
      timestamp,
      timestamp,
    );
    sqlite.query(`
      INSERT INTO organizations (id, slug, name, status, created_at_ms)
      VALUES (?, 'personal', ?, 'active', ?)
    `).run(accountId, canonicalPayload.emailNormalized, timestamp);
    sqlite.query(`
      INSERT INTO organization_memberships (
        organization_id, account_id, role, created_at_ms
      ) VALUES (?, ?, 'owner', ?)
    `).run(accountId, accountId, timestamp);
    sqlite.query(`
      INSERT INTO dashboards (
        id, organization_id, slug, name, status, created_at_ms
      ) VALUES (?, ?, 'default', 'Personal', 'active', ?)
    `).run(coordinatorId, accountId, timestamp);
    sqlite.query(`
      INSERT INTO dashboard_memberships (
        dashboard_id, account_id, role, created_at_ms
      ) VALUES (?, ?, 'admin', ?)
    `).run(coordinatorId, accountId, timestamp);
    return { accountId, coordinatorId };
  });
}
