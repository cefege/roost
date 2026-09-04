/**
 * Owns account-bound coordinator relocation redemption coverage.
 * Bun discovers this suite, which calls the shared bootstrap handler fixtures.
 * It depends on relocation handler wiring and persisted account and device state.
 */
import { afterEach, describe, expect, test } from "bun:test";
import { create } from "@bufbuild/protobuf";
import { Code, type HandlerContext } from "@connectrpc/connect";
import { AuthRedeemCoordinatorRelocationRequestSchema } from "@roost/shared/proto/coordinator_pb";
import { makeRelocationHandlers } from "../src/connect/handlers-relocation.ts";
import type { ConnectDeps } from "../src/connect/router.ts";
import {
  authorizeAccountDevice,
  connectFailure,
  createBootstrapHandlerHarnessOwner,
  makeKey,
  type TestKey,
} from "./bootstrap-token-handlers-fixture.ts";

const { cleanupHarnesses, openHarness } = createBootstrapHandlerHarnessOwner();
afterEach(async () => {
  await cleanupHarnesses();
});

const INVALID_RELOCATION = {
  code: Code.Unauthenticated,
  message: "invalid or expired relocation token",
};

describe("account-bound relocation redemption", () => {
  async function setup() {
    const h = await openHarness();
    const delegator = await makeKey();
    await authorizeAccountDevice(h, delegator);
    const handoff = {
      handoff_id: "handoff",
      target_url: "https://target.example:4102",
      role: "TARGET",
      phase: "COMMITTED",
    } as const;
    let tokenValid = true;
    const deps = {
      ...h.deps,
      move: { current: () => handoff },
      coordKey: {
        verifyRelocation: async () => {
          if (!tokenValid) throw new Error("secret verification detail");
          const now = Math.floor(Date.now() / 1_000);
          return {
            aud: "roost-coordinator-relocation",
            sub: delegator.fingerprint,
            iat: now,
            exp: now + 60,
            handoff_id: handoff.handoff_id,
            target_url: handoff.target_url,
            jti: "relocation-jti",
          };
        },
      },
    } as unknown as ConnectDeps;
    return {
      h,
      delegator,
      handlers: makeRelocationHandlers(deps),
      invalidateToken() { tokenValid = false; },
    };
  }

  function request(key: TestKey) {
    return create(AuthRedeemCoordinatorRelocationRequestSchema, {
      token: "signed-token",
      sshPubkeyB64: key.b64,
      label: "relocated browser",
    });
  }

  test("records the delegator account and rejects replay uniformly", async () => {
    const fixture = await setup();
    const destination = await makeKey();
    await fixture.handlers.authRedeemCoordinatorRelocation(request(destination), {} as HandlerContext);
    expect(await fixture.h.db.selectFrom("coordinator_relocation_redemptions")
      .select(["jti", "account_id", "used_by_fp", "delegated_by_fp"])
      .executeTakeFirst()).toEqual({
      jti: "relocation-jti",
      account_id: fixture.h.tenant.accountId,
      used_by_fp: destination.fingerprint,
      delegated_by_fp: fixture.delegator.fingerprint,
    });
    expect(await connectFailure(async () => fixture.handlers.authRedeemCoordinatorRelocation(
      request(destination), {} as HandlerContext,
    ))).toEqual(INVALID_RELOCATION);
  });

  test("invalid, revoked, worker, foreign, and orphan-key states are uniform", async () => {
    const invalid = await setup();
    invalid.invalidateToken();
    expect(await connectFailure(async () => invalid.handlers.authRedeemCoordinatorRelocation(
      request(await makeKey()), {} as HandlerContext,
    ))).toEqual(INVALID_RELOCATION);

    const revoked = await setup();
    await revoked.h.db.insertInto("authorized_key_revocations").values({
      fingerprint: revoked.delegator.fingerprint,
      revoked_at_ms: Date.now(),
      revoked_by_fp: "test",
      reason: "test",
    }).execute();
    expect(await connectFailure(async () => revoked.handlers.authRedeemCoordinatorRelocation(
      request(await makeKey()), {} as HandlerContext,
    ))).toEqual(INVALID_RELOCATION);

    const orphan = await setup();
    const orphanKey = await makeKey();
    await orphan.h.db.insertInto("authorized_keys").values({
      fingerprint: orphanKey.fingerprint,
      public_key: orphanKey.raw,
      label: "orphan",
      added_at: Date.now(),
    }).execute();
    expect(await connectFailure(async () => orphan.handlers.authRedeemCoordinatorRelocation(
      request(orphanKey), {} as HandlerContext,
    ))).toEqual(INVALID_RELOCATION);

    const workerCollision = await setup();
    const worker = await makeKey();
    const now = Date.now();
    await workerCollision.h.db.insertInto("authorized_keys").values({
      fingerprint: worker.fingerprint,
      public_key: worker.raw,
      label: "worker",
      added_at: now,
    }).execute();
    await workerCollision.h.db.insertInto("workers").values({
      fp: worker.fingerprint,
      dashboard_id: workerCollision.h.tenant.dashboardId,
      label: "worker",
      os: "linux",
      git_sha: null,
      host_metrics_json: null,
      registered_at_ms: now,
      last_seen_ms: now,
    }).execute();
    expect(await connectFailure(async () => workerCollision.handlers.authRedeemCoordinatorRelocation(
      request(worker), {} as HandlerContext,
    ))).toEqual(INVALID_RELOCATION);

    const foreign = await setup();
    const foreignKey = await makeKey();
    await foreign.h.db.insertInto("accounts").values({
      id: "foreign-account",
      email_normalized: "foreign@example.test",
      password_hash: null,
      status: "active",
      created_at_ms: now,
      password_changed_at_ms: null,
    }).execute();
    await foreign.h.db.insertInto("authorized_keys").values({
      fingerprint: foreignKey.fingerprint,
      public_key: foreignKey.raw,
      label: "foreign",
      added_at: now,
    }).execute();
    await foreign.h.db.insertInto("account_devices").values({
      fingerprint: foreignKey.fingerprint,
      account_id: "foreign-account",
      added_at_ms: now,
      last_seen_at_ms: now,
    }).execute();
    expect(await connectFailure(async () => foreign.handlers.authRedeemCoordinatorRelocation(
      request(foreignKey), {} as HandlerContext,
    ))).toEqual(INVALID_RELOCATION);
  });

  test("unexpected SQLite failures expose only the constant unavailable error", async () => {
    const fixture = await setup();
    fixture.h.sqlite.exec(`
      CREATE TRIGGER relocation_test_failure
      BEFORE INSERT ON coordinator_relocation_redemptions
      BEGIN
        SELECT RAISE(ABORT, 'secret sqlite detail');
      END
    `);
    const failure = await connectFailure(async () => fixture.handlers.authRedeemCoordinatorRelocation(
      request(await makeKey()), {} as HandlerContext,
    ));
    expect(failure).toEqual({
      code: Code.Unavailable,
      message: "relocation redemption unavailable",
    });
    expect(failure.message).not.toContain("sqlite");
    expect(failure.message).not.toContain("secret");
  });
});
