/**
 * Owns browser and worker grant-redemption coverage for device revocation.
 * This split keeps concurrent redemption cases reviewable without changing their setup.
 * It depends on the shared revocation fixture and coordinator authentication schemas.
 */
import { afterEach, describe, expect, test } from "bun:test";
import { create } from "@bufbuild/protobuf";
import { Code } from "@connectrpc/connect";
import {
  AuthRedeemBrowserRequestSchema,
  AuthRedeemWorkerRequestSchema,
} from "@roost/shared/proto/coordinator_pb";
import { bootstrapTokenDigest } from "../src/bootstrap-tokens.ts";
import {
  type DeviceKey,
  createDeviceRevocationHarnessOwner,
  key,
  token,
  unauthCtx,
} from "./device-revocation-fixture.ts";

const { cleanupHarnesses, openHarness: harness } = createDeviceRevocationHarnessOwner();
afterEach(cleanupHarnesses);

describe("authorized device lifecycle", () => {
  test("two browser redemptions race: exactly one claims and installs a key", async () => {
    const h = await harness();
    const first = await key();
    const second = await key();
    await token(h, "one-shot", null);
    const redeem = (device: DeviceKey) => h.handlers.authRedeemBrowser(
      create(AuthRedeemBrowserRequestSchema, {
        token: "one-shot",
        sshPubkeyB64: device.b64,
        label: "racer",
      }),
      unauthCtx("203.0.113.1", false),
    );
    const settled = await Promise.allSettled([redeem(first), redeem(second)]);
    expect(settled.filter((result) => result.status === "fulfilled")).toHaveLength(1);
    expect(settled.filter((result) => result.status === "rejected")).toHaveLength(1);
    const authorized = await h.db.selectFrom("authorized_keys").select("fingerprint").execute();
    expect(authorized).toHaveLength(1);
    expect([first.fingerprint, second.fingerprint]).toContain(authorized[0]?.fingerprint);
    const claimed = await h.db.selectFrom("bootstrap_tokens").select("used_by_fp")
      .where("token_hash", "=", await bootstrapTokenDigest("one-shot")).executeTakeFirstOrThrow();
    expect(claimed.used_by_fp).toBe(authorized[0]?.fingerprint);
  });

  test("tailnet address never substitutes for an explicit browser grant", async () => {
    const h = await harness();
    const added = await key();
    const tailnet = unauthCtx("100.101.102.103", false);

    await expect(h.handlers.authRedeemBrowser(
      create(AuthRedeemBrowserRequestSchema, {
        token: "missing-grant",
        sshPubkeyB64: added.b64,
        label: "new browser",
      }),
      tailnet,
    )).rejects.toMatchObject({
      code: Code.Unauthenticated,
      rawMessage: "invalid or expired token",
    });
    expect(await h.db.selectFrom("authorized_keys")
      .select("fingerprint")
      .where("fingerprint", "=", added.fingerprint)
      .executeTakeFirst()).toBeUndefined();

    await token(h, "browser-grant", null);
    await h.handlers.authRedeemBrowser(
      create(AuthRedeemBrowserRequestSchema, {
        token: "browser-grant",
        sshPubkeyB64: added.b64,
        label: "new browser",
      }),
      tailnet,
    );

    expect(await h.db.selectFrom("account_devices")
      .select(["fingerprint", "account_id"])
      .where("fingerprint", "=", added.fingerprint)
      .executeTakeFirst()).toEqual({
        fingerprint: added.fingerprint,
        account_id: h.tenant.accountId,
      });
  });

  test("two worker redemptions race: exactly one claims and registers", async () => {
    const h = await harness();
    const first = await key();
    const second = await key();
    await token(h, "worker-one-shot", null, "worker");
    const redeem = (device: DeviceKey) => h.handlers.authRedeemWorker(
      create(AuthRedeemWorkerRequestSchema, {
        token: "worker-one-shot",
        sshPubkeyB64: device.b64,
        label: "worker-racer",
        os: "linux",
      }),
      unauthCtx("203.0.113.1", false),
    );
    const settled = await Promise.allSettled([redeem(first), redeem(second)]);
    expect(settled.filter((result) => result.status === "fulfilled")).toHaveLength(1);
    expect(settled.filter((result) => result.status === "rejected")).toHaveLength(1);
    const workers = await h.db.selectFrom("workers").select("fp").execute();
    expect(workers).toHaveLength(1);
    expect([first.fingerprint, second.fingerprint]).toContain(workers[0]?.fp);
    const claimed = await h.db.selectFrom("bootstrap_tokens").select("used_by_fp")
      .where("token_hash", "=", await bootstrapTokenDigest("worker-one-shot")).executeTakeFirstOrThrow();
    expect(claimed.used_by_fp).toBe(workers[0]?.fp);
  });

});
