/**
 * Owns minting and redemption coverage for scoped bootstrap grants.
 * Bun discovers this suite, which calls the shared bootstrap handler fixtures.
 * It depends on auth handler wiring and persisted token, account, and worker state.
 */
import { afterEach, describe, expect, test } from "bun:test";
import { create } from "@bufbuild/protobuf";
import {
  Code,
  createContextValues,
  type HandlerContext,
} from "@connectrpc/connect";
import {
  AuthMintBootstrapRequestSchema,
  AuthRedeemBrowserRequestSchema,
  AuthRedeemBrowserResponseSchema,
  AuthRedeemWorkerRequestSchema,
} from "@roost/shared/proto/coordinator_pb";
import {
  bootstrapTokenDigest,
  mintBootstrapToken,
  type BootstrapTokenKind,
} from "../src/bootstrap-tokens.ts";
import { callerKey, dashboardActorKey } from "../src/connect/auth-interceptor.ts";
import {
  authorizeAccountDevice,
  connectFailure,
  createBootstrapHandlerHarnessOwner,
  type Harness,
  makeKey,
  type TestKey,
} from "./bootstrap-token-handlers-fixture.ts";

const { cleanupHarnesses, openHarness } = createBootstrapHandlerHarnessOwner();
afterEach(async () => {
  await cleanupHarnesses();
});

function actorContext(h: Harness, key: TestKey): HandlerContext {
  const values = createContextValues();
  values.set(callerKey, {
    kind: "account-device",
    fingerprint: key.fingerprint,
    label: "actor",
    accountId: h.tenant.accountId,
  });
  values.set(dashboardActorKey, {
    accountId: h.tenant.accountId,
    organizationId: h.tenant.organizationId,
    dashboardId: h.tenant.dashboardId,
    organizationRole: "owner",
    dashboardRole: "admin",
    deviceFingerprint: key.fingerprint,
  });
  return { values } as unknown as HandlerContext;
}

async function grant(h: Harness, kind: BootstrapTokenKind): Promise<string> {
  return (await mintBootstrapToken(h.db, {
    kind,
    label: `${kind} grant`,
    accountId: h.tenant.accountId,
    dashboardId: h.tenant.dashboardId,
    mintedByFp: null,
  })).token;
}

function browserRequest(token: string, key: TestKey) {
  return create(AuthRedeemBrowserRequestSchema, {
    token,
    sshPubkeyB64: key.b64,
    label: "browser",
  });
}

function workerRequest(token: string, key: TestKey) {
  return create(AuthRedeemWorkerRequestSchema, {
    token,
    sshPubkeyB64: key.b64,
    label: "worker",
    os: "linux",
    gitSha: "test",
  });
}

const INVALID_GRANT = {
  code: Code.Unauthenticated,
  message: "invalid or expired token",
};

describe("digest-only scoped bootstrap grants", () => {
  test("authenticated mint stores only the actor-scoped digest", async () => {
    const h = await openHarness();
    const actor = await makeKey();
    await authorizeAccountDevice(h, actor);

    const response = await h.handlers.authMintBootstrap(create(AuthMintBootstrapRequestSchema, {
      kind: "worker",
      label: "new worker",
    }), actorContext(h, actor));
    if (response.token === undefined) throw new Error("mint response omitted token");
    const digest = await bootstrapTokenDigest(response.token);
    const row = h.sqlite.query(`
      SELECT token_hash, account_id, dashboard_id, minted_by_fp
      FROM bootstrap_tokens
    `).get() as {
      token_hash: string;
      account_id: string;
      dashboard_id: string;
      minted_by_fp: string;
    };
    expect(row).toEqual({
      token_hash: digest,
      account_id: h.tenant.accountId,
      dashboard_id: h.tenant.dashboardId,
      minted_by_fp: actor.fingerprint,
    });
    const columns = h.sqlite.query("PRAGMA table_info(bootstrap_tokens)").all() as Array<{ name: string }>;
    expect(columns.map((column) => column.name)).not.toContain("token");
  });

  test("worker redemption permits only an exact same-key lost-response retry", async () => {
    const h = await openHarness();
    const first = await makeKey();
    const competitor = await makeKey();
    const token = await grant(h, "worker");

    const workerResponse = await h.handlers.authRedeemWorker(
      workerRequest(token, first), {} as HandlerContext,
    );
    expect(workerResponse).toMatchObject({ fingerprint: first.fingerprint, label: "worker" });
    await h.handlers.authRedeemWorker(workerRequest(token, first), {} as HandlerContext);
    expect(await connectFailure(async () =>
      h.handlers.authRedeemWorker(workerRequest(token, competitor), {} as HandlerContext),
    )).toEqual(INVALID_GRANT);
    expect(await h.db.selectFrom("workers").select(["fp", "dashboard_id"]).execute()).toEqual([{
      fp: first.fingerprint,
      dashboard_id: h.tenant.dashboardId,
    }]);
  });

  test("browser redemption permits retry but not a competitor or a new grant for that principal", async () => {
    const h = await openHarness();
    const first = await makeKey();
    const competitor = await makeKey();
    const token = await grant(h, "browser");

    const browserResponse = await h.handlers.authRedeemBrowser(
      browserRequest(token, first), {} as HandlerContext,
    );
    expect(browserResponse).toEqual(create(AuthRedeemBrowserResponseSchema, {}));
    await h.handlers.authRedeemBrowser(browserRequest(token, first), {} as HandlerContext);
    expect(await connectFailure(async () =>
      h.handlers.authRedeemBrowser(browserRequest(token, competitor), {} as HandlerContext),
    )).toEqual(INVALID_GRANT);
    expect(await connectFailure(async () =>
      h.handlers.authRedeemBrowser(browserRequest(await grant(h, "browser"), first), {} as HandlerContext),
    )).toEqual(INVALID_GRANT);
  });

  test("revoked, inactive, foreign, and inconsistent grants share one error", async () => {
    const revoked = await openHarness();
    const revokedKey = await makeKey();
    await revoked.db.insertInto("authorized_key_revocations").values({
      fingerprint: revokedKey.fingerprint,
      revoked_at_ms: Date.now(),
      revoked_by_fp: "test",
      reason: "test",
    }).execute();
    expect(await connectFailure(async () => revoked.handlers.authRedeemBrowser(
      browserRequest(await grant(revoked, "browser"), revokedKey), {} as HandlerContext,
    ))).toEqual(INVALID_GRANT);

    const inactive = await openHarness();
    const inactiveToken = await grant(inactive, "browser");
    await inactive.db.updateTable("accounts").set({ status: "disabled" })
      .where("id", "=", inactive.tenant.accountId).execute();
    expect(await connectFailure(async () => inactive.handlers.authRedeemBrowser(
      browserRequest(inactiveToken, await makeKey()), {} as HandlerContext,
    ))).toEqual(INVALID_GRANT);

    const foreign = await openHarness();
    const foreignToken = await grant(foreign, "browser");
    await foreign.db.deleteFrom("dashboard_memberships")
      .where("dashboard_id", "=", foreign.tenant.dashboardId)
      .where("account_id", "=", foreign.tenant.accountId).execute();
    expect(await connectFailure(async () => foreign.handlers.authRedeemBrowser(
      browserRequest(foreignToken, await makeKey()), {} as HandlerContext,
    ))).toEqual(INVALID_GRANT);

    const inconsistent = await openHarness();
    const worker = await makeKey();
    const inconsistentToken = await grant(inconsistent, "worker");
    await inconsistent.handlers.authRedeemWorker(
      workerRequest(inconsistentToken, worker), {} as HandlerContext,
    );
    await inconsistent.db.deleteFrom("workers").where("fp", "=", worker.fingerprint).execute();
    expect(await connectFailure(async () => inconsistent.handlers.authRedeemWorker(
      workerRequest(inconsistentToken, worker), {} as HandlerContext,
    ))).toEqual(INVALID_GRANT);
  });

  test("managed browser redemption remains explicitly unavailable", async () => {
    const h = await openHarness(true);
    expect(await connectFailure(async () => h.handlers.authRedeemBrowser(
      browserRequest("not-a-grant", await makeKey()), {} as HandlerContext,
    ))).toEqual({
      code: Code.PermissionDenied,
      message: "legacy browser authorization is unavailable in managed mode",
    });
  });
});
