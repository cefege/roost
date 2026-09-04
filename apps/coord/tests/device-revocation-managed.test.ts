/**
 * Owns managed-account device authorization, logout, and rollback coverage.
 * This split keeps SaaS-only lifecycle cases separate from self-hosted revocation paths.
 * It depends on the shared revocation fixture and coordinator authentication schemas.
 */
import { afterEach, describe, expect, test } from "bun:test";
import { create } from "@bufbuild/protobuf";
import { Code } from "@connectrpc/connect";
import {
  AuthLogoutRequestSchema,
  AuthRedeemBrowserRequestSchema,
  DevicesListRequestSchema,
  DevicesRevokeRequestSchema,
  PairCreateRequestSchema,
} from "@roost/shared/proto/coordinator_pb";
import { bootstrapTokenDigest } from "../src/bootstrap-tokens.ts";
import {
  accountCtx,
  addAccountDevice,
  createDeviceRevocationHarnessOwner,
  authCtx,
  key,
  token,
  unauthCtx,
  workerCtx,
} from "./device-revocation-fixture.ts";

const { cleanupHarnesses, openHarness: harness } = createDeviceRevocationHarnessOwner();
afterEach(cleanupHarnesses);

describe("authorized device lifecycle", () => {
  test("managed device operations are account-scoped and reject worker and legacy principals", async () => {
    const h = await harness(true);
    const first = await key();
    const foreign = await key();
    await addAccountDevice(h.db, "managed-first", first, "first");
    await addAccountDevice(h.db, "managed-foreign", foreign, "foreign");

    const response = await h.handlers.devicesList(
      create(DevicesListRequestSchema),
      accountCtx(first.fingerprint, "managed-first"),
    );
    const devices = response.devices;
    if (devices === undefined) throw new Error("expected device list");
    expect(devices.map((device) => device.fingerprint)).toEqual([first.fingerprint]);
    await expect(h.handlers.devicesRevoke(
      create(DevicesRevokeRequestSchema, { fingerprint: foreign.fingerprint }),
      accountCtx(first.fingerprint, "managed-first"),
    )).rejects.toMatchObject({ code: Code.NotFound });
    await expect(h.handlers.devicesList(
      create(DevicesListRequestSchema),
      workerCtx("worker-principal", "managed-dashboard"),
    )).rejects.toMatchObject({ code: Code.Unauthenticated });
    await expect(h.handlers.devicesList(
      create(DevicesListRequestSchema),
      authCtx(first.fingerprint),
    )).rejects.toMatchObject({ code: Code.Unauthenticated });
    expect(await h.db.selectFrom("authorized_keys").select("fingerprint")
      .where("fingerprint", "=", foreign.fingerprint).executeTakeFirst())
      .toEqual({ fingerprint: foreign.fingerprint });
  });

  test("managed handlers reject self-hosted browser enrollment paths", async () => {
    const h = await harness(true);
    await expect(h.handlers.authRedeemBrowser(
      create(AuthRedeemBrowserRequestSchema),
      unauthCtx("127.0.0.1", true),
    )).rejects.toMatchObject({ code: Code.PermissionDenied });
    await expect(h.handlers.pairCreate(
      create(PairCreateRequestSchema),
      unauthCtx("127.0.0.1", true),
    )).rejects.toMatchObject({ code: Code.PermissionDenied });
  });

  test("logout atomically removes device credentials and invokes socket callbacks after commit", async () => {
    const h = await harness(true);
    const current = await key();
    const peer = await key();
    const accountId = "logout-account";
    const peerAccountId = "logout-peer-account";
    const organizationId = "logout-organization";
    const dashboardId = "logout-dashboard";
    const now = Date.now();
    await addAccountDevice(h.db, accountId, current, "current");
    await addAccountDevice(h.db, peerAccountId, peer, "peer");
    await h.db.insertInto("organizations").values({
      id: organizationId,
      slug: "logout",
      name: "Logout",
      status: "active",
      created_at_ms: now,
    }).execute();
    await h.db.insertInto("dashboards").values({
      id: dashboardId,
      organization_id: organizationId,
      slug: "logout",
      name: "Logout",
      status: "active",
      created_at_ms: now,
    }).execute();
    await h.db.insertInto("dashboard_memberships").values({
      dashboard_id: dashboardId,
      account_id: accountId,
      role: "admin",
      created_at_ms: now,
    }).execute();
    await token(h, "logout-delegated", current.fingerprint, "worker", dashboardId);
    await token(h, "unowned-bootstrap", null, "worker", dashboardId);
    await h.db.insertInto("push_subscriptions").values([
      {
        dashboard_id: dashboardId,
        viewer_fp: current.fingerprint,
        endpoint: "https://push.example/current-1",
        p256dh: "p1",
        auth: "a1",
        created_at_ms: now,
      },
      {
        dashboard_id: dashboardId,
        viewer_fp: current.fingerprint,
        endpoint: "https://push.example/current-2",
        p256dh: "p2",
        auth: "a2",
        created_at_ms: now,
      },
      {
        dashboard_id: dashboardId,
        viewer_fp: peer.fingerprint,
        endpoint: "https://push.example/peer",
        p256dh: "p3",
        auth: "a3",
        created_at_ms: now,
      },
    ]).execute();

    const response = await h.handlers.authLogout(
      create(AuthLogoutRequestSchema),
      accountCtx(current.fingerprint, accountId),
    );
    expect(response.ok).toBe(true);
    expect(await h.db.selectFrom("authorized_keys").select("fingerprint").execute())
      .toEqual([{ fingerprint: peer.fingerprint }]);
    expect(await h.db.selectFrom("account_devices").select("fingerprint").execute())
      .toEqual([{ fingerprint: peer.fingerprint }]);
    expect(await h.db.selectFrom("bootstrap_tokens").select("token_hash").execute())
      .toEqual([{ token_hash: await bootstrapTokenDigest("unowned-bootstrap") }]);
    expect(await h.db.selectFrom("push_subscriptions").select("viewer_fp").execute())
      .toEqual([{ viewer_fp: peer.fingerprint }]);
    expect(await h.db.selectFrom("authorized_key_revocations")
      .select(["fingerprint", "reason"]).execute())
      .toEqual([{ fingerprint: current.fingerprint, reason: "browser-logout" }]);
    expect(h.callbackStates).toEqual([{ keys: 0, devices: 0, pushes: 0 }]);
    expect(h.revoked).toEqual([current.fingerprint]);
    expect(h.dashboardRevocations).toEqual([{
      dashboardId,
      fingerprint: current.fingerprint,
    }]);
  });

  test("logout rolls back every cleanup row when the final key deletion fails", async () => {
    const h = await harness(true);
    const current = await key();
    const accountId = "logout-rollback-account";
    await addAccountDevice(h.db, accountId, current, "current");
    await token(h, "logout-rollback-token", current.fingerprint);
    await h.db.insertInto("push_subscriptions").values({
      dashboard_id: h.tenant.dashboardId,
      viewer_fp: current.fingerprint,
      endpoint: "https://push.example/rollback",
      p256dh: "p",
      auth: "a",
      created_at_ms: Date.now(),
    }).execute();
    h.sqlite.exec(`
      CREATE TRIGGER abort_logout_key_delete
      BEFORE DELETE ON authorized_keys
      WHEN OLD.fingerprint = '${current.fingerprint}'
      BEGIN
        SELECT RAISE(ABORT, 'forced logout rollback');
      END
    `);

    await expect(h.handlers.authLogout(
      create(AuthLogoutRequestSchema),
      accountCtx(current.fingerprint, accountId),
    )).rejects.toThrow("forced logout rollback");
    expect(await h.db.selectFrom("authorized_keys").select("fingerprint").execute())
      .toEqual([{ fingerprint: current.fingerprint }]);
    expect(await h.db.selectFrom("account_devices").select("fingerprint").execute())
      .toEqual([{ fingerprint: current.fingerprint }]);
    expect(await h.db.selectFrom("bootstrap_tokens").select("token_hash").execute())
      .toEqual([{ token_hash: await bootstrapTokenDigest("logout-rollback-token") }]);
    expect(await h.db.selectFrom("push_subscriptions").select("viewer_fp").execute())
      .toEqual([{ viewer_fp: current.fingerprint }]);
    expect(await h.db.selectFrom("authorized_key_revocations").selectAll().execute()).toEqual([]);
    expect(h.revoked).toEqual([]);
    expect(h.callbackStates).toEqual([]);
  });
});
