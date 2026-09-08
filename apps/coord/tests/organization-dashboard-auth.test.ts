import { afterAll, beforeAll, describe, expect, test } from "bun:test";
import { create } from "@bufbuild/protobuf";
import { createContextValues, type HandlerContext } from "@connectrpc/connect";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import {
  AuthCoordIdentityRequestSchema,
  AuthDashboardAccessRequestSchema,
} from "@roost/shared/proto/coordinator_pb";
import { openDb, type KyselyDB } from "../src/db/connection.ts";
import { runMigrations } from "../src/db/migrate.ts";
import {
  callerKey,
  dashboardActorKey,
  requestedDashboardIdKey,
  getDashboardAccessSnapshot,
  makeAuthInterceptor,
  listenerTrustKey,
  requireDashboardAdmin,
  requireOrganizationAdmin,
  resolveDashboardActor,
  resolveCallerPrincipal,
  type DashboardActor,
} from "../src/connect/auth-interceptor.ts";
import { makeAuthHandlers } from "../src/connect/handlers-auth.ts";
import type { ConnectDeps } from "../src/connect/router.ts";
import { auditBus } from "../src/buses.ts";

let workdir: string;
let closeDb: () => Promise<void>;
let db: KyselyDB;

const accountA = "account-a";
const accountB = "account-b";
const accountDisabled = "account-disabled";
const fpA = "fp-account-a";
const fpB = "fp-account-b";
const fpDisabled = "fp-account-disabled";
const fpWorker = "fp-worker";
const fpLegacy = "fp-legacy";
const orgA = "org-a";
const orgOnly = "org-only";
const dashboardA = "dashboard-a";
const dashboardB = "dashboard-b";
const dashboardSuspended = "dashboard-suspended";

function contextFor(
  fingerprint: string,
  requestedDashboardId?: string,
): HandlerContext {
  const values = createContextValues();
  values.set(callerKey, {
    kind: "account-device",
    fingerprint,
    label: "test device",
    accountId: fingerprint === fpB
      ? accountB
      : fingerprint === fpDisabled
        ? accountDisabled
        : accountA,
  });
  if (requestedDashboardId !== undefined) {
    values.set(requestedDashboardIdKey, requestedDashboardId);
  }
  return { values } as unknown as HandlerContext;
}

function actor(overrides: Partial<DashboardActor> = {}): DashboardActor {
  return {
    accountId: accountA,
    organizationId: orgA,
    dashboardId: dashboardA,
    organizationRole: "owner",
    dashboardRole: "admin",
    deviceFingerprint: fpA,
    ...overrides,
  };
}

beforeAll(async () => {
  workdir = mkdtempSync(join(tmpdir(), "roost-dashboard-actor-"));
  const opened = openDb(join(workdir, "test.db"));
  db = opened.db;
  closeDb = opened.close;
  await runMigrations(opened.sqlite);

  const now = Date.now();
  await db.insertInto("authorized_keys").values([
    { fingerprint: fpA, public_key: new Uint8Array(32), label: "A", added_at: now },
    { fingerprint: fpB, public_key: new Uint8Array(32), label: "B", added_at: now },
    { fingerprint: fpDisabled, public_key: new Uint8Array(32), label: "disabled", added_at: now },
    { fingerprint: fpWorker, public_key: new Uint8Array(32), label: "worker", added_at: now },
    { fingerprint: fpLegacy, public_key: new Uint8Array(32), label: "legacy", added_at: now },
  ]).execute();
  await db.insertInto("accounts").values([
    { id: accountA, email_normalized: "a@example.test", status: "active", created_at_ms: now },
    { id: accountB, email_normalized: "b@example.test", status: "active", created_at_ms: now },
    { id: accountDisabled, email_normalized: "disabled@example.test", status: "disabled", created_at_ms: now },
  ]).execute();
  await db.insertInto("account_devices").values([
    { fingerprint: fpA, account_id: accountA, added_at_ms: now, last_seen_at_ms: now },
    { fingerprint: fpB, account_id: accountB, added_at_ms: now, last_seen_at_ms: now },
    { fingerprint: fpDisabled, account_id: accountDisabled, added_at_ms: now, last_seen_at_ms: now },
  ]).execute();
  await db.insertInto("organizations").values([
    { id: orgA, slug: "org-a", name: "Organization A", status: "active", created_at_ms: now },
    { id: orgOnly, slug: "org-only", name: "Organization Only", status: "active", created_at_ms: now + 1 },
  ]).execute();
  await db.insertInto("organization_memberships").values([
    { organization_id: orgA, account_id: accountA, role: "owner", created_at_ms: now },
    { organization_id: orgA, account_id: accountB, role: "admin", created_at_ms: now },
    // This organization membership deliberately has no dashboard grant.
    { organization_id: orgOnly, account_id: accountA, role: "admin", created_at_ms: now },
    { organization_id: orgA, account_id: accountDisabled, role: "member", created_at_ms: now },
  ]).execute();
  await db.insertInto("dashboards").values([
    { id: dashboardA, organization_id: orgA, slug: "dashboard-a", name: "Dashboard A", status: "active", created_at_ms: now },
    { id: dashboardB, organization_id: orgA, slug: "dashboard-b", name: "Dashboard B", status: "active", created_at_ms: now + 1 },
    { id: dashboardSuspended, organization_id: orgA, slug: "dashboard-suspended", name: "Suspended", status: "suspended", created_at_ms: now + 2 },
  ]).execute();
  await db.insertInto("workers").values({
    fp: fpWorker,
    dashboard_id: dashboardA,
    label: "worker",
    os: "linux",
    git_sha: null,
    host_metrics_json: null,
    registered_at_ms: now,
    last_seen_ms: now,
    reachable_addr: null,
  }).execute();
  await db.insertInto("dashboard_memberships").values([
    { dashboard_id: dashboardA, account_id: accountA, role: "admin", created_at_ms: now },
    { dashboard_id: dashboardB, account_id: accountB, role: "member", created_at_ms: now },
    { dashboard_id: dashboardSuspended, account_id: accountA, role: "admin", created_at_ms: now },
    { dashboard_id: dashboardA, account_id: accountDisabled, role: "member", created_at_ms: now },
  ]).execute();
});

afterAll(async () => {
  await closeDb?.();
  rmSync(workdir, { recursive: true, force: true });
});

describe("dashboard actor resolution", () => {
  test("resolves only an active device/account/org/dashboard membership", async () => {
    await expect(resolveDashboardActor(db, fpA, dashboardA)).resolves.toEqual(actor());
  });

  test("resolves account, worker, and legacy principals without authority fallback", async () => {
    const verified = (fingerprint: string, label: string) => ({ fingerprint, label });
    await expect(resolveCallerPrincipal(db, verified(fpA, "A"))).resolves.toEqual({
      kind: "account-device",
      fingerprint: fpA,
      label: "A",
      accountId: accountA,
    });
    await expect(resolveCallerPrincipal(db, verified(fpWorker, "worker"))).resolves.toEqual({
      kind: "worker",
      fingerprint: fpWorker,
      label: "worker",
      dashboardId: dashboardA,
    });
    await expect(resolveCallerPrincipal(db, verified(fpDisabled, "disabled"))).resolves.toBeNull();
    await expect(resolveCallerPrincipal(db, verified(fpLegacy, "legacy"))).resolves.toEqual({
      kind: "legacy-self-hosted",
      fingerprint: fpLegacy,
      label: "legacy",
    });
  });

  test("organization membership alone does not grant dashboard access", async () => {
    const [foreign, missing, suspended, disabled] = await Promise.all([
      resolveDashboardActor(db, fpA, dashboardB),
      resolveDashboardActor(db, fpA, "does-not-exist"),
      resolveDashboardActor(db, fpA, dashboardSuspended),
      resolveDashboardActor(db, fpDisabled, dashboardA),
    ]);
    // All failed selected scopes are intentionally indistinguishable.
    expect([foreign, missing, suspended, disabled]).toEqual([null, null, null, null]);
  });

  test("lists only active scopes and falls back from a stale selection", async () => {
    const handlers = makeAuthHandlers({ db } as unknown as ConnectDeps);
    const response = await handlers.authDashboardAccess(
      create(AuthDashboardAccessRequestSchema, {}),
      contextFor(fpA, dashboardB),
    );
    expect(response.accountId).toBe(accountA);
    expect(response.selectedDashboardId).toBe(dashboardA);
    expect(response.organizations?.map((organization) => organization.id)).toEqual([orgA, orgOnly]);
    expect(response.dashboards?.map((dashboard) => dashboard.id)).toEqual([dashboardA]);
    expect(response.capabilities).toEqual([
      "dashboard:member",
      "dashboard:admin",
      "organization:admin",
      "organization:owner",
    ]);

    const snapshot = await getDashboardAccessSnapshot(db, fpA);
    expect(snapshot?.dashboards.map((dashboard) => dashboard.id)).toEqual([dashboardA]);
  });

  test("dashboard and organization role guards have distinct limits", () => {
    const memberValues = createContextValues();
    memberValues.set(callerKey, {
      kind: "account-device",
      fingerprint: fpA,
      label: "test device",
      accountId: accountA,
    });
    memberValues.set(dashboardActorKey, actor({
      organizationRole: "member",
      dashboardRole: "member",
    }));
    expect(() => requireDashboardAdmin(memberValues)).toThrow("dashboard admin required");
    expect(() => requireOrganizationAdmin(memberValues)).toThrow("organization admin required");

    const organizationAdminValues = createContextValues();
    organizationAdminValues.set(callerKey, {
      kind: "account-device",
      fingerprint: fpA,
      label: "test device",
      accountId: accountA,
    });
    organizationAdminValues.set(dashboardActorKey, actor({
      organizationRole: "admin",
      dashboardRole: "member",
    }));
    expect(requireOrganizationAdmin(organizationAdminValues).organizationRole).toBe("admin");
  });

});
