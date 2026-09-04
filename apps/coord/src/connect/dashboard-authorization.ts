// Dashboard authority comes only from active account, organization, and
// dashboard memberships resolved from persisted rows. Both selected-scope and
// bootstrap-list queries share these role guards to fail closed on corrupt data.

import type { KyselyDB } from "../db/connection.ts";

export type OrganizationRole = "owner" | "admin" | "member";
export type DashboardRole = "admin" | "member";

/**
 * The server-confirmed tenant identity for one request. The dashboard header
 * is only an input to resolver lookup; handlers use this value, never it.
 */
export interface DashboardActor {
  accountId: string;
  organizationId: string;
  dashboardId: string;
  organizationRole: OrganizationRole;
  dashboardRole: DashboardRole;
  deviceFingerprint: string;
}

export interface AccessibleOrganization {
  id: string;
  slug: string;
  name: string;
  role: OrganizationRole;
}

export interface AccessibleDashboard {
  id: string;
  organizationId: string;
  slug: string;
  name: string;
  organizationRole: OrganizationRole;
  dashboardRole: DashboardRole;
}

export interface DashboardAccessSnapshot {
  accountId: string;
  organizations: AccessibleOrganization[];
  dashboards: AccessibleDashboard[];
}

function isOrganizationRole(value: string): value is OrganizationRole {
  return value === "owner" || value === "admin" || value === "member";
}

function isDashboardRole(value: string): value is DashboardRole {
  return value === "admin" || value === "member";
}

export function isDashboardActor(value: unknown): value is DashboardActor {
  if (!value || typeof value !== "object") return false;
  const actor = value as Partial<DashboardActor>;
  return typeof actor.accountId === "string"
    && typeof actor.organizationId === "string"
    && typeof actor.dashboardId === "string"
    && typeof actor.deviceFingerprint === "string"
    && isOrganizationRole(actor.organizationRole ?? "")
    && isDashboardRole(actor.dashboardRole ?? "");
}

/**
 * Resolves an authenticated device and a client-requested dashboard into a
 * server-owned actor. Every predicate is part of the one lookup so missing,
 * foreign, disabled, suspended, and deleted scope resolves to the same null.
 */
export async function resolveDashboardActor(
  db: KyselyDB,
  deviceFingerprint: string,
  dashboardId: string,
): Promise<DashboardActor | null> {
  const row = await db
    .selectFrom("account_devices as ad")
    .innerJoin("accounts as a", "a.id", "ad.account_id")
    .innerJoin("dashboard_memberships as dm", "dm.account_id", "ad.account_id")
    .innerJoin("dashboards as d", "d.id", "dm.dashboard_id")
    .innerJoin("organizations as o", "o.id", "d.organization_id")
    .innerJoin("organization_memberships as om", "om.account_id", "ad.account_id")
    .select([
      "ad.account_id as accountId",
      "d.organization_id as organizationId",
      "d.id as dashboardId",
      "om.role as organizationRole",
      "dm.role as dashboardRole",
    ])
    .where("ad.fingerprint", "=", deviceFingerprint)
    .where("d.id", "=", dashboardId)
    .whereRef("om.organization_id", "=", "d.organization_id")
    .where("a.status", "=", "active")
    .where("o.status", "=", "active")
    .where("d.status", "=", "active")
    .executeTakeFirst();
  if (!row
    || !isOrganizationRole(row.organizationRole)
    || !isDashboardRole(row.dashboardRole)) {
    return null;
  }
  return {
    accountId: row.accountId,
    organizationId: row.organizationId,
    dashboardId: row.dashboardId,
    organizationRole: row.organizationRole,
    dashboardRole: row.dashboardRole,
    deviceFingerprint,
  };
}

/**
 * Returns all active scopes reachable by an active device account. This is
 * intentionally separate from selected-actor resolution: bootstrapping may
 * carry no selection or a stale one, and the server chooses a valid fallback.
 */
export async function getDashboardAccessSnapshot(
  db: KyselyDB,
  deviceFingerprint: string,
): Promise<DashboardAccessSnapshot | null> {
  const account = await db
    .selectFrom("account_devices as ad")
    .innerJoin("accounts as a", "a.id", "ad.account_id")
    .select("ad.account_id as accountId")
    .where("ad.fingerprint", "=", deviceFingerprint)
    .where("a.status", "=", "active")
    .executeTakeFirst();
  if (!account) return null;

  const [organizationRows, dashboardRows] = await Promise.all([
    db
      .selectFrom("organization_memberships as om")
      .innerJoin("organizations as o", "o.id", "om.organization_id")
      .select([
        "o.id as id",
        "o.slug as slug",
        "o.name as name",
        "om.role as role",
      ])
      .where("om.account_id", "=", account.accountId)
      .where("o.status", "=", "active")
      .orderBy("o.created_at_ms", "asc")
      .orderBy("o.id", "asc")
      .execute(),
    db
      .selectFrom("dashboard_memberships as dm")
      .innerJoin("dashboards as d", "d.id", "dm.dashboard_id")
      .innerJoin("organizations as o", "o.id", "d.organization_id")
      .innerJoin("organization_memberships as om", "om.account_id", "dm.account_id")
      .select([
        "d.id as id",
        "d.organization_id as organizationId",
        "d.slug as slug",
        "d.name as name",
        "om.role as organizationRole",
        "dm.role as dashboardRole",
      ])
      .where("dm.account_id", "=", account.accountId)
      .whereRef("om.organization_id", "=", "d.organization_id")
      .where("o.status", "=", "active")
      .where("d.status", "=", "active")
      .orderBy("o.created_at_ms", "asc")
      .orderBy("d.created_at_ms", "asc")
      .orderBy("d.id", "asc")
      .execute(),
  ]);

  return {
    accountId: account.accountId,
    organizations: organizationRows.flatMap((row) =>
      isOrganizationRole(row.role)
        ? [{ id: row.id, slug: row.slug, name: row.name, role: row.role }]
        : [],
    ),
    dashboards: dashboardRows.flatMap((row) =>
      isOrganizationRole(row.organizationRole) && isDashboardRole(row.dashboardRole)
        ? [{
          id: row.id,
          organizationId: row.organizationId,
          slug: row.slug,
          name: row.name,
          organizationRole: row.organizationRole,
          dashboardRole: row.dashboardRole,
        }]
        : [],
    ),
  };
}
