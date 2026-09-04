// Owns native password login behavior involving device-key identity and browser guards.
// The coord test runner exercises native and worker auth through isolated migrated databases.
// It is split from credential-outcome coverage so each suite remains below the file cap.
// It depends on the shared login fixture, auth handlers, and fingerprinting.
import { afterEach, beforeAll, describe, expect, test } from "bun:test";
import { create } from "@bufbuild/protobuf";
import { Code, ConnectError, createContextValues } from "@connectrpc/connect";
import { fingerprintOf } from "@roost/shared/fingerprint";
import { NATIVE_PASSWORD_ARGON2ID } from "@roost/shared/native-credentials";
import { AuthRedeemWorkerRequestSchema } from "@roost/shared/proto/coordinator_pb";
import { callerKey, optionalAccountDevice, requireAccountDevice } from "../src/connect/auth-interceptor.ts";
import { makeAuthHandlers } from "../src/connect/handlers-auth.ts";
import { bootstrapTokenDigest } from "../src/bootstrap-tokens.ts";
import {
  ACCOUNT_ID,
  DASHBOARD_ID,
  NOW,
  OTHER_ACCOUNT_ID,
  createNativePasswordLoginFixtures,
  encodedKey,
  expectInvalidCredentials,
  expectNoDeviceWrites,
  handlerContext,
  loginRequest,
  type Harness,
} from "./native-password-login-fixture.ts";

const {
  cleanupHarnesses,
  createHarness,
  getPasswordHash,
  initializePasswordHash,
  provisionOwner,
} = createNativePasswordLoginFixtures();

beforeAll(initializePasswordHash);
afterEach(cleanupHarnesses);

describe("managed native password login", () => {
  test("rejects malformed, revoked, worker, and cross-account device keys generically", async () => {
    const malformed = await createHarness();
    await provisionOwner(malformed.db);
    await expectInvalidCredentials(() => malformed.handlers.authPasswordLogin(
      loginRequest("not-an-ed25519-key"),
      handlerContext(),
    ));
    await expectNoDeviceWrites(malformed.db);

    const revoked = await createHarness();
    await provisionOwner(revoked.db);
    const revokedKey = encodedKey(20);
    const revokedFingerprint = await fingerprintOf(revokedKey.bytes);
    await revoked.db.insertInto("authorized_key_revocations").values({
      fingerprint: revokedFingerprint,
      revoked_at_ms: NOW,
      revoked_by_fp: "operator",
      reason: "test",
    }).execute();
    await expectInvalidCredentials(() => revoked.handlers.authPasswordLogin(
      loginRequest(revokedKey.b64),
      handlerContext(),
    ));
    expect(await revoked.db.selectFrom("account_devices").select("fingerprint").execute()).toEqual([]);

    const worker = await createHarness();
    await provisionOwner(worker.db);
    const workerKey = encodedKey(21);
    const workerFingerprint = await fingerprintOf(workerKey.bytes);
    await worker.db.insertInto("authorized_keys").values({
      fingerprint: workerFingerprint,
      public_key: workerKey.bytes,
      label: "worker",
      added_at: NOW,
    }).execute();
    await worker.db.insertInto("workers").values({
      fp: workerFingerprint,
      dashboard_id: DASHBOARD_ID,
      label: "worker",
      os: "linux",
      git_sha: null,
      host_metrics_json: null,
      registered_at_ms: NOW,
      last_seen_ms: NOW,
      reachable_addr: null,
      keeper_stale: null,
    }).execute();
    await expectInvalidCredentials(() => worker.handlers.authPasswordLogin(
      loginRequest(workerKey.b64),
      handlerContext(),
    ));
    expect(await worker.db.selectFrom("account_devices").select("fingerprint").execute()).toEqual([]);

    const other = await createHarness();
    await provisionOwner(other.db);
    const otherKey = encodedKey(22);
    const otherFingerprint = await fingerprintOf(otherKey.bytes);
    await other.db.insertInto("accounts").values({
      id: OTHER_ACCOUNT_ID,
      email_normalized: "other@example.test",
      password_hash: getPasswordHash(),
      status: "active",
      created_at_ms: NOW,
      password_changed_at_ms: NOW,
    }).execute();
    await other.db.insertInto("authorized_keys").values({
      fingerprint: otherFingerprint,
      public_key: otherKey.bytes,
      label: "other",
      added_at: NOW,
    }).execute();
    await other.db.insertInto("account_devices").values({
      fingerprint: otherFingerprint,
      account_id: OTHER_ACCOUNT_ID,
      added_at_ms: NOW,
      last_seen_at_ms: NOW,
    }).execute();
    await expectInvalidCredentials(() => other.handlers.authPasswordLogin(
      loginRequest(otherKey.b64),
      handlerContext(),
    ));
    expect(await other.db.selectFrom("account_devices").select("account_id")
      .where("fingerprint", "=", otherFingerprint).executeTakeFirstOrThrow()).toEqual({
      account_id: OTHER_ACCOUNT_ID,
    });
  });

  test("does not bind a key when the verified password hash changes before the transaction", async () => {
    let harness!: Harness;
    harness = await createHarness({
      verifyPassword: async (password, hash) => {
        const matches = await Bun.password.verify(password, hash);
        await harness.db.updateTable("accounts")
          .set({ password_hash: await Bun.password.hash("replacement password", NATIVE_PASSWORD_ARGON2ID) })
          .where("id", "=", ACCOUNT_ID)
          .execute();
        return matches;
      },
    });
    await provisionOwner(harness.db);

    await expectInvalidCredentials(() => harness.handlers.authPasswordLogin(
      loginRequest(encodedKey(30).b64),
      handlerContext(),
    ));
    await expectNoDeviceWrites(harness.db);
  });

  test("leaves self-hosted passwordless pairing mode unchanged", async () => {
    const harness = await createHarness();
    await provisionOwner(harness.db);
    harness.deps.cfg.saasMode = false;
    await expectInvalidCredentials(() => harness.handlers.authPasswordLogin(
      loginRequest(encodedKey(31).b64),
      handlerContext(),
    ));
    await expectNoDeviceWrites(harness.db);
  });

  test("prevents a password-bound device key from later enrolling as a worker", async () => {
    const harness = await createHarness();
    await provisionOwner(harness.db);
    const key = encodedKey(40);
    const fingerprint = await fingerprintOf(key.bytes);
    await harness.handlers.authPasswordLogin(loginRequest(key.b64), handlerContext());
    await harness.db.insertInto("bootstrap_tokens").values({
      token_hash: await bootstrapTokenDigest("worker-bootstrap-token"),
      account_id: ACCOUNT_ID,
      dashboard_id: DASHBOARD_ID,
      kind: "worker",
      label: "worker",
      created_at_ms: NOW,
      expires_at_ms: NOW + 60_000,
      used_at_ms: null,
      used_by_fp: null,
      minted_by_fp: null,
    }).execute();

    try {
      await makeAuthHandlers(harness.deps).authRedeemWorker(create(AuthRedeemWorkerRequestSchema, {
        token: "worker-bootstrap-token",
        sshPubkeyB64: key.b64,
        label: "worker",
        os: "linux",
      }), handlerContext());
      throw new Error("expected worker enrollment denial");
    } catch (error) {
      expect(error).toBeInstanceOf(ConnectError);
      expect((error as ConnectError).code).toBe(Code.Unauthenticated);
    }
    expect(await harness.db.selectFrom("workers").select("fp")
      .where("fp", "=", fingerprint).executeTakeFirst()).toBeUndefined();
    expect(await harness.db.selectFrom("bootstrap_tokens").select("used_at_ms")
      .where("token_hash", "=", await bootstrapTokenDigest("worker-bootstrap-token"))
      .executeTakeFirstOrThrow()).toEqual({ used_at_ms: null });
  });

  test("rejects worker principals at required and optional browser guards", () => {
    const values = createContextValues();
    values.set(callerKey, {
      kind: "worker",
      fingerprint: "worker-device",
      label: "worker",
      dashboardId: DASHBOARD_ID,
    });
    for (const resolve of [requireAccountDevice, optionalAccountDevice]) {
      try {
        resolve(values);
        throw new Error("expected worker-principal denial");
      } catch (error) {
        expect(error).toBeInstanceOf(ConnectError);
        expect((error as ConnectError).code).toBe(Code.Unauthenticated);
      }
    }
  });
});
