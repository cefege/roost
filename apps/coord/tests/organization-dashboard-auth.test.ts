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
    { id: accountA, email_normalized: "a@example.test", password_hash: null, status: "active", created_at_ms: now, password_changed_at_ms: null },
    { id: accountB, email_normalized: "b@example.test", password_hash: null, status: "active", created_at_ms: now, password_changed_at_ms: null },
    { id: accountDisabled, email_normalized: "disabled@example.test", password_hash: null, status: "disabled", created_at_ms: now, password_changed_at_ms: null },
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
    keeper_stale: null,
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
    const managed = { saasMode: true } as Parameters<typeof resolveCallerPrincipal>[1];
    const selfHosted = { saasMode: false } as Parameters<typeof resolveCallerPrincipal>[1];
    const verified = (fingerprint: string, label: string) => ({ fingerprint, label });
    await expect(resolveCallerPrincipal(db, managed, verified(fpA, "A"))).resolves.toEqual({
      kind: "account-device",
      fingerprint: fpA,
      label: "A",
      accountId: accountA,
    });
    await expect(resolveCallerPrincipal(db, managed, verified(fpWorker, "worker"))).resolves.toEqual({
      kind: "worker",
      fingerprint: fpWorker,
      label: "worker",
      dashboardId: dashboardA,
    });
    await expect(resolveCallerPrincipal(db, managed, verified(fpDisabled, "disabled"))).resolves.toBeNull();
    await expect(resolveCallerPrincipal(db, managed, verified(fpLegacy, "legacy"))).resolves.toBeNull();
    await expect(resolveCallerPrincipal(db, selfHosted, verified(fpLegacy, "legacy"))).resolves.toEqual({
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

  test("reports the exact managed instance on either accepting listener", async () => {
    const instanceId = "11111111-1111-4111-8111-111111111111";
    const handlers = makeAuthHandlers({
      cfg: {
        saasMode: true,
        managedContainer: true,
        instanceId,
      },
    } as unknown as ConnectDeps);

    for (const [listener, publicListener] of [
      ["direct", false],
      ["public-edge", true],
    ] as const) {
      const values = createContextValues();
      values.set(listenerTrustKey, listener);
      const response = await handlers.authCoordIdentity(
        create(AuthCoordIdentityRequestSchema, {}),
        { values } as unknown as HandlerContext,
      );
      expect("fingerprintHex" in response).toBe(false);
      expect(response).toMatchObject({
        saasMode: true,
        instanceId,
        publicListener,
      });
    }
  });
  test("does not audit a request that began before managed bootstrap committed", async () => {
    const dir = mkdtempSync(join(tmpdir(), "roost-auth-bootstrap-audit-"));
    const opened = openDb(join(dir, "coord.db"));
    await runMigrations(opened.sqlite);
    const interceptor = makeAuthInterceptor({
      db: opened.db,
      cfg: { saasMode: true },
      jwtCache: {},
    } as unknown as Parameters<typeof makeAuthInterceptor>[0]);
    const entered = Promise.withResolvers<void>();
    const release = Promise.withResolvers<void>();
    const invoke = interceptor as unknown as (
      next: (request: unknown) => Promise<Response>,
    ) => (request: unknown) => Promise<Response>;
    const pending = invoke(async () => {
      entered.resolve();
      await release.promise;
      return new Response();
    })({
      method: { name: "SessionsList" },
      service: { typeName: "roost.v1.CoordinatorService" },
      header: new Headers(),
      contextValues: createContextValues(),
    });
    await entered.promise;
    await opened.db.insertInto("accounts").values({
      id: "bootstrap-account",
      email_normalized: "owner@example.com",
      password_hash: null,
      status: "active",
      created_at_ms: 1,
      password_changed_at_ms: null,
    }).execute();
    release.resolve();
    await pending;
    const audit = opened.sqlite.query("SELECT COUNT(*) AS count FROM audit_log").get() as { count: number };
    expect(audit.count).toBe(0);
    await opened.close();
    rmSync(dir, { recursive: true, force: true });
  });

  test("scopes actorless managed-container audits to the instance dashboard", async () => {
    const dir = mkdtempSync(join(tmpdir(), "roost-auth-instance-audit-"));
    const opened = openDb(join(dir, "coord.db"));
    const instanceId = "11111111-1111-4111-8111-111111111111";
    try {
      await runMigrations(opened.sqlite);
      await opened.db.insertInto("accounts").values({
        id: "bootstrap-account",
        email_normalized: "owner@example.com",
        password_hash: "hash",
        status: "active",
        created_at_ms: 1,
        password_changed_at_ms: 1,
      }).execute();
      await opened.db.insertInto("organizations").values({
        id: "bootstrap-account",
        slug: "personal",
        name: "owner@example.com",
        status: "active",
        created_at_ms: 1,
      }).execute();
      await opened.db.insertInto("dashboards").values({
        id: instanceId,
        organization_id: "bootstrap-account",
        slug: "default",
        name: "Personal",
        status: "active",
        created_at_ms: 1,
      }).execute();
      const interceptor = makeAuthInterceptor({
        db: opened.db,
        cfg: {
          saasMode: true,
          managedContainer: true,
          instanceId,
        },
        jwtCache: {},
      } as unknown as Parameters<typeof makeAuthInterceptor>[0]);
      const invoke = interceptor as unknown as (
        next: (request: unknown) => Promise<Response>,
      ) => (request: unknown) => Promise<Response>;
      const auditWritten = Promise.withResolvers<void>();
      const unsubscribe = auditBus.subscribe(() => auditWritten.resolve());
      try {
        await invoke(async () => new Response())({
          method: { name: "AuthPasswordLogin" },
          service: { typeName: "roost.v1.CoordinatorService" },
          header: new Headers(),
          contextValues: createContextValues(),
        });
        await auditWritten.promise;
        const audit = opened.sqlite.query(
          "SELECT dashboard_id FROM audit_log LIMIT 1",
        ).get() as { dashboard_id: string | null } | null;
        expect(audit).toEqual({ dashboard_id: instanceId });
      } finally {
        unsubscribe();
      }
    } finally {
      await opened.close();
      rmSync(dir, { recursive: true, force: true });
    }
  });

});
