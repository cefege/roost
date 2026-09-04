// Account device enrollment writes key ownership and dashboard membership in one
// transaction so credential handlers cannot admit a partial topology. Callers
// deliberately translate every rejection into their own uniform public result.

import type { Transaction } from "kysely";
import type { DB } from "../db/schema.ts";

/** A deliberately content-free failure: public credential handlers must map
 * every enrollment/topology rejection to their own uniform auth result. */
export class AccountDeviceEnrollmentError extends Error {
  constructor() {
    super("account device enrollment rejected");
    this.name = "AccountDeviceEnrollmentError";
  }
}

export interface AccountDeviceEnrollmentInput {
  accountId: string;
  fingerprint: string;
  publicKey: Uint8Array;
  label: string;
  now: number;
  expectedDashboardId?: string;
}

function reject(): never {
  throw new AccountDeviceEnrollmentError();
}

function sameBytes(left: Uint8Array, right: Uint8Array): boolean {
  if (left.byteLength !== right.byteLength) return false;
  let different = 0;
  for (let index = 0; index < left.byteLength; index++) {
    different |= left[index]! ^ right[index]!;
  }
  return different === 0;
}

/**
 * Enrolls one browser key only after proving the managed coordinator still has
 * the exact single-owner/single-dashboard topology for the target account.
 * Callers own the surrounding transaction and refresh JWT caches after commit.
 */
export async function enrollAccountDevice(
  trx: Transaction<DB>,
  input: AccountDeviceEnrollmentInput,
): Promise<string> {
  const account = await trx.selectFrom("accounts")
    .select("id")
    .where("id", "=", input.accountId)
    .where("status", "=", "active")
    .executeTakeFirst();
  if (!account) reject();

  const organizations = await trx.selectFrom("organization_memberships as om")
    .innerJoin("organizations as o", "o.id", "om.organization_id")
    .select([
      "o.id as organizationId",
      "om.role as organizationRole",
    ])
    .where("om.account_id", "=", input.accountId)
    .where("o.status", "=", "active")
    .limit(2)
    .execute();
  if (organizations.length !== 1 || organizations[0]!.organizationRole !== "owner") reject();
  const organizationId = organizations[0]!.organizationId;

  const dashboards = await trx.selectFrom("dashboard_memberships as dm")
    .innerJoin("dashboards as d", "d.id", "dm.dashboard_id")
    .innerJoin("organizations as o", "o.id", "d.organization_id")
    .select([
      "d.id as dashboardId",
      "d.organization_id as organizationId",
      "dm.role as dashboardRole",
    ])
    .where("dm.account_id", "=", input.accountId)
    .where("d.status", "=", "active")
    .where("o.status", "=", "active")
    .limit(2)
    .execute();
  if (
    dashboards.length !== 1
    || dashboards[0]!.organizationId !== organizationId
    || dashboards[0]!.dashboardRole !== "admin"
    || (input.expectedDashboardId !== undefined
      && dashboards[0]!.dashboardId !== input.expectedDashboardId)
  ) reject();
  const dashboardId = dashboards[0]!.dashboardId;

  // An isolated coordinator must not hide another live dashboard or another
  // member behind a topology that merely happens to give this account access.
  const [organizationMembers, activeDashboards, dashboardMembers] = await Promise.all([
    trx.selectFrom("organization_memberships")
      .select("account_id")
      .where("organization_id", "=", organizationId)
      .limit(2)
      .execute(),
    trx.selectFrom("dashboards")
      .select("id")
      .where("organization_id", "=", organizationId)
      .where("status", "=", "active")
      .limit(2)
      .execute(),
    trx.selectFrom("dashboard_memberships")
      .select("account_id")
      .where("dashboard_id", "=", dashboardId)
      .limit(2)
      .execute(),
  ]);
  if (
    organizationMembers.length !== 1
    || organizationMembers[0]!.account_id !== input.accountId
    || activeDashboards.length !== 1
    || activeDashboards[0]!.id !== dashboardId
    || dashboardMembers.length !== 1
    || dashboardMembers[0]!.account_id !== input.accountId
  ) reject();

  const [revocation, worker, existingDevice, existingKey] = await Promise.all([
    trx.selectFrom("authorized_key_revocations")
      .select("fingerprint")
      .where("fingerprint", "=", input.fingerprint)
      .executeTakeFirst(),
    trx.selectFrom("workers")
      .select("fp")
      .where("fp", "=", input.fingerprint)
      .executeTakeFirst(),
    trx.selectFrom("account_devices")
      .select("account_id")
      .where("fingerprint", "=", input.fingerprint)
      .executeTakeFirst(),
    trx.selectFrom("authorized_keys")
      .select("public_key")
      .where("fingerprint", "=", input.fingerprint)
      .executeTakeFirst(),
  ]);
  if (
    revocation
    || worker
    || (existingDevice && existingDevice.account_id !== input.accountId)
    || (existingKey && (!existingDevice || !sameBytes(existingKey.public_key, input.publicKey)))
  ) reject();

  await trx.insertInto("authorized_keys").values({
    fingerprint: input.fingerprint,
    public_key: input.publicKey,
    label: input.label,
    added_at: input.now,
  }).onConflict((conflict) => conflict.column("fingerprint").doUpdateSet({
    label: input.label,
  })).execute();

  if (existingDevice) {
    await trx.updateTable("account_devices")
      .set({ last_seen_at_ms: input.now })
      .where("fingerprint", "=", input.fingerprint)
      .where("account_id", "=", input.accountId)
      .execute();
  } else {
    await trx.insertInto("account_devices").values({
      fingerprint: input.fingerprint,
      account_id: input.accountId,
      added_at_ms: input.now,
      last_seen_at_ms: input.now,
    }).execute();
  }

  return dashboardId;
}
