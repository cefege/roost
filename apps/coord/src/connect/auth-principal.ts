// JWT verification yields only a key identity; this module resolves that key to
// exactly one persisted application principal. Re-reading authority rows here
// prevents deleted, dual-role, or inactive identities from entering RPC context.

import type { CoordConfig } from "@roost/shared/config";
import type { KyselyDB } from "../db/connection.ts";

interface PrincipalIdentity {
  fingerprint: string;
  label: string;
}

export interface AccountDevicePrincipal extends PrincipalIdentity {
  kind: "account-device";
  accountId: string;
}

export interface WorkerPrincipal extends PrincipalIdentity {
  kind: "worker";
  dashboardId: string;
}

export interface LegacySelfHostedPrincipal extends PrincipalIdentity {
  kind: "legacy-self-hosted";
}

export type Caller =
  | AccountDevicePrincipal
  | WorkerPrincipal
  | LegacySelfHostedPrincipal;

/** Browser authority. Legacy keys remain browser-capable only in self-hosted mode. */
export type AccountDeviceCaller =
  | AccountDevicePrincipal
  | LegacySelfHostedPrincipal;


/**
 * Converts a cryptographically verified key into its persisted application
 * principal. The authorized-key row is re-read as part of this lookup so a
 * key deleted between signature verification and principal resolution is not
 * admitted.
 */
export async function resolveCallerPrincipal(
  db: KyselyDB,
  cfg: CoordConfig,
  verified: { fingerprint: string; label: string },
): Promise<Caller | null> {
  const row = await db
    .selectFrom("authorized_keys as key")
    .leftJoin("account_devices as device", "device.fingerprint", "key.fingerprint")
    .leftJoin("accounts as account", "account.id", "device.account_id")
    .leftJoin("workers as worker", "worker.fp", "key.fingerprint")
    .leftJoin("dashboards as workerDashboard", "workerDashboard.id", "worker.dashboard_id")
    .leftJoin(
      "organizations as workerOrganization",
      "workerOrganization.id",
      "workerDashboard.organization_id",
    )
    .select([
      "device.fingerprint as deviceFingerprint",
      "device.account_id as accountId",
      "account.status as accountStatus",
      "worker.fp as workerFingerprint",
      "worker.dashboard_id as workerDashboardId",
      "worker.deleted_at_ms as workerDeletedAt",
      "workerDashboard.status as workerDashboardStatus",
      "workerOrganization.status as workerOrganizationStatus",
    ])
    .where("key.fingerprint", "=", verified.fingerprint)
    .executeTakeFirst();
  if (!row) return null;

  const hasAccountDevice = row.deviceFingerprint !== null;
  const hasWorker = row.workerFingerprint !== null;
  // A key must never acquire two kinds of authority. Refuse corrupt or
  // transition-state rows rather than choosing whichever join happened first.
  if (hasAccountDevice && hasWorker) return null;

  if (hasAccountDevice) {
    if (!row.accountId || row.accountStatus !== "active") return null;
    return {
      kind: "account-device",
      fingerprint: verified.fingerprint,
      label: verified.label,
      accountId: row.accountId,
    };
  }
  if (hasWorker) {
    if (
      row.workerDeletedAt !== null
      || !row.workerDashboardId
      || row.workerDashboardStatus !== "active"
      || row.workerOrganizationStatus !== "active"
    ) return null;
    return {
      kind: "worker",
      fingerprint: verified.fingerprint,
      label: verified.label,
      dashboardId: row.workerDashboardId,
    };
  }
  if (cfg.saasMode) return null;
  return {
    kind: "legacy-self-hosted",
    fingerprint: verified.fingerprint,
    label: verified.label,
  };
}
