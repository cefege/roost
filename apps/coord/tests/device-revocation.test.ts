/**
 * Owns self-hosted device listing, revocation, and key-rotation coverage.
 * This split keeps the core lifecycle cases separate from redemption and managed flows.
 * It depends on the shared revocation fixture and coordinator device schemas.
 */
import { afterEach, describe, expect, test } from "bun:test";
import { create } from "@bufbuild/protobuf";
import { Code } from "@connectrpc/connect";
import {
  DevicesListRequestSchema,
  DevicesRevokeRequestSchema,
  DevicesRotateCurrentRequestSchema,
} from "@roost/shared/proto/coordinator_pb";
import {
  authCtx,
  authorize,
  createDeviceRevocationHarnessOwner,
  dashboardAdminCtx,
  key,
  token,
  unauthCtx,
  workerDeleteDashboardId,
} from "./device-revocation-fixture.ts";

const { cleanupHarnesses, openHarness: harness } = createDeviceRevocationHarnessOwner();
afterEach(cleanupHarnesses);

describe("authorized device lifecycle", () => {
  test("lists non-worker devices newest-first and marks the caller", async () => {
    const h = await harness();
    const self = await key();
    const peer = await key();
    await authorize(h.db, self, "self");
    await authorize(h.db, peer, "peer");
    await h.db.updateTable("authorized_keys").set({ added_at: 2 })
      .where("fingerprint", "=", self.fingerprint).execute();
    await h.db.updateTable("authorized_keys").set({ added_at: 1 })
      .where("fingerprint", "=", peer.fingerprint).execute();
    await h.db.insertInto("workers").values({
      fp: peer.fingerprint,
      dashboard_id: h.tenant.dashboardId,
      label: "worker",
      os: "linux",
      git_sha: null,
      host_metrics_json: null,
      registered_at_ms: 1,
      last_seen_ms: 1,
      reachable_addr: null,
      keeper_stale: null,
    }).execute();

    const response = await h.handlers.devicesList(
      create(DevicesListRequestSchema),
      authCtx(self.fingerprint),
    );
    const devices = response.devices ?? [];
    expect(devices.map((device) => device.fingerprint)).toEqual([self.fingerprint]);
    expect(devices[0]?.isSelf).toBe(true);
  });

  test("rejects authenticated self-revoke and unauthenticated proxy callers", async () => {
    const h = await harness();
    const self = await key();
    await authorize(h.db, self, "self");
    const request = create(DevicesRevokeRequestSchema, { fingerprint: self.fingerprint });
    await expect(h.handlers.devicesRevoke(request, authCtx(self.fingerprint)))
      .rejects.toMatchObject({ code: Code.InvalidArgument });
    await expect(h.handlers.devicesRevoke(request, unauthCtx("100.64.0.2", false)))
      .rejects.toMatchObject({ code: Code.PermissionDenied });
  });

  test("on-host recovery permanently revokes a device and all uncertain tokens", async () => {
    const h = await harness();
    const lost = await key();
    await authorize(h.db, lost, "lost");
    await token(h, "delegated", lost.fingerprint);
    await token(h, "legacy", null);

    const response = await h.handlers.devicesRevoke(
      create(DevicesRevokeRequestSchema, { fingerprint: lost.fingerprint }),
      unauthCtx("127.0.0.1", true),
    );
    expect(response.ok).toBe(true);
    expect(await h.db.selectFrom("authorized_keys").selectAll().execute()).toEqual([]);
    expect(await h.db.selectFrom("bootstrap_tokens").selectAll().execute()).toEqual([]);
    expect(await h.db.selectFrom("authorized_key_revocations").selectAll().executeTakeFirst())
      .toMatchObject({ fingerprint: lost.fingerprint, revoked_by_fp: "on-host-recovery" });
    expect(h.revoked).toEqual([lost.fingerprint]);
  });

  test("scoped device revocation invokes the dashboard lifecycle callback after commit", async () => {
    const h = await harness();
    const lost = await key();
    const now = Date.now();
    const accountId = "revoked-device-account";
    const organizationId = "revoked-device-organization";
    await h.db.insertInto("accounts").values({
      id: accountId,
      email_normalized: "revoked-device@example.test",
      password_hash: null,
      status: "active",
      created_at_ms: now,
      password_changed_at_ms: null,
    }).execute();
    await h.db.insertInto("organizations").values({
      id: organizationId,
      slug: "revoked-device-org",
      name: "Revoked device",
      status: "active",
      created_at_ms: now,
    }).execute();
    await h.db.insertInto("organization_memberships").values({
      organization_id: organizationId,
      account_id: accountId,
      role: "member",
      created_at_ms: now,
    }).execute();
    await h.db.insertInto("dashboards").values({
      id: workerDeleteDashboardId,
      organization_id: organizationId,
      slug: "revoked-device-dashboard",
      name: "Revoked device",
      status: "active",
      created_at_ms: now,
    }).execute();
    await h.db.insertInto("dashboard_memberships").values({
      dashboard_id: workerDeleteDashboardId,
      account_id: accountId,
      role: "member",
      created_at_ms: now,
    }).execute();
    await authorize(h.db, lost, "lost");
    await h.db.insertInto("account_devices").values({
      fingerprint: lost.fingerprint,
      account_id: accountId,
      added_at_ms: now,
      last_seen_at_ms: now,
    }).execute();
    await h.handlers.devicesRevoke(
      create(DevicesRevokeRequestSchema, { fingerprint: lost.fingerprint }),
      dashboardAdminCtx(),
    );
    expect(h.revoked).toEqual([lost.fingerprint]);
    expect(h.dashboardRevocations).toEqual([{
      dashboardId: workerDeleteDashboardId,
      fingerprint: lost.fingerprint,
    }]);
  });

  test("rotates atomically and invalidates the old key's delegated tokens", async () => {
    const h = await harness();
    const old = await key();
    const replacement = await key();
    await authorize(h.db, old, "old");
    await token(h, "delegated", old.fingerprint);

    const response = await h.handlers.devicesRotateCurrent(
      create(DevicesRotateCurrentRequestSchema, {
        sshPubkeyB64: replacement.b64,
        label: "replacement",
      }),
      authCtx(old.fingerprint),
    );
    expect(response.fingerprint).toBe(replacement.fingerprint);
    expect(await h.db.selectFrom("authorized_keys").select("fingerprint").execute())
      .toEqual([{ fingerprint: replacement.fingerprint }]);
    expect(await h.db.selectFrom("authorized_key_revocations").select("fingerprint").execute())
      .toEqual([{ fingerprint: old.fingerprint }]);
    expect(await h.db.selectFrom("bootstrap_tokens").selectAll().execute()).toEqual([]);
    expect(h.revoked).toEqual([old.fingerprint]);
  });

  test("rotation rejects malformed SSH wire keys instead of slicing arbitrary bytes", async () => {
    const h = await harness();
    const current = await key();
    await authorize(h.db, current, "current");
    const malformedWire = Buffer.from(new Uint8Array(51)).toString("base64");
    await expect(h.handlers.devicesRotateCurrent(
      create(DevicesRotateCurrentRequestSchema, {
        sshPubkeyB64: malformedWire,
        label: "malformed",
      }),
      authCtx(current.fingerprint),
    )).rejects.toMatchObject({ code: Code.InvalidArgument });
    expect(await h.db.selectFrom("authorized_keys").select("fingerprint").execute())
      .toEqual([{ fingerprint: current.fingerprint }]);
  });

});
