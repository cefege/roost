// Owns managed native password login success and credential-rejection coverage.
// The coord test runner exercises native auth through isolated migrated databases.
// Device-key identity edge cases live in the sibling suite to keep both files focused.
// It depends on the shared native password login fixture and fingerprinting.
import { afterEach, beforeAll, describe, expect, test } from "bun:test";
import { fingerprintOf } from "@roost/shared/fingerprint";
import {
  ACCOUNT_ID,
  DASHBOARD_ID,
  EMAIL,
  NOW,
  ORGANIZATION_ID,
  PASSWORD,
  createNativePasswordLoginFixtures,
  encodedKey,
  expectInvalidCredentials,
  expectNoDeviceWrites,
  handlerContext,
  loginRequest,
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
  test("normalizes email and atomically binds the browser key to the owner dashboard", async () => {
    const harness = await createHarness();
    await provisionOwner(harness.db);
    const key = encodedKey(7);
    const fingerprint = await fingerprintOf(key.bytes);

    const response = await harness.handlers.authPasswordLogin(
      loginRequest(key.b64, { email: "  Owner@Example.Test  ", label: "  Owner laptop  " }),
      handlerContext(),
    );

    expect(response.dashboardId).toBe(DASHBOARD_ID);
    expect(await harness.db.selectFrom("authorized_keys").selectAll()
      .where("fingerprint", "=", fingerprint).executeTakeFirst()).toMatchObject({
      fingerprint,
      label: "Owner laptop",
    });
    expect(await harness.db.selectFrom("account_devices").selectAll()
      .where("fingerprint", "=", fingerprint).executeTakeFirst()).toMatchObject({
      fingerprint,
      account_id: ACCOUNT_ID,
    });
    const account = await harness.db.selectFrom("accounts").select("password_hash")
      .where("id", "=", ACCOUNT_ID).executeTakeFirstOrThrow();
    expect(account.password_hash).not.toBe(PASSWORD);
    expect(await Bun.password.verify(PASSWORD, account.password_hash!)).toBe(true);

    const second = await harness.handlers.authPasswordLogin(
      loginRequest(key.b64, { label: "Renamed browser" }),
      handlerContext(),
    );
    expect(second.dashboardId).toBe(DASHBOARD_ID);
    expect(await harness.db.selectFrom("account_devices").select("fingerprint").execute()).toHaveLength(1);
    expect(await harness.db.selectFrom("authorized_keys").select("label")
      .where("fingerprint", "=", fingerprint).executeTakeFirstOrThrow()).toEqual({
      label: "Renamed browser",
    });
  });

  test("collapses unknown, wrong, disabled, passwordless, and unscoped accounts to one outcome", async () => {
    const unknown = await createHarness();
    await expectInvalidCredentials(() => unknown.handlers.authPasswordLogin(
      loginRequest(encodedKey(10).b64, { email: "missing@example.test" }),
      handlerContext(),
    ));
    await expectNoDeviceWrites(unknown.db);

    const wrong = await createHarness();
    await provisionOwner(wrong.db);
    await expectInvalidCredentials(() => wrong.handlers.authPasswordLogin(
      loginRequest(encodedKey(11).b64, { password: "this is not the password" }),
      handlerContext(),
    ));
    await expectNoDeviceWrites(wrong.db);

    const disabled = await createHarness();
    await provisionOwner(disabled.db, { status: "disabled" });
    await expectInvalidCredentials(() => disabled.handlers.authPasswordLogin(
      loginRequest(encodedKey(12).b64),
      handlerContext(),
    ));
    await expectNoDeviceWrites(disabled.db);

    const passwordless = await createHarness();
    await provisionOwner(passwordless.db, { storedHash: null });
    await expectInvalidCredentials(() => passwordless.handlers.authPasswordLogin(
      loginRequest(encodedKey(13).b64),
      handlerContext(),
    ));
    await expectNoDeviceWrites(passwordless.db);

    const unscoped = await createHarness();
    await unscoped.db.insertInto("accounts").values({
      id: ACCOUNT_ID,
      email_normalized: EMAIL,
      password_hash: getPasswordHash(),
      status: "active",
      created_at_ms: NOW,
      password_changed_at_ms: NOW,
    }).execute();
    await expectInvalidCredentials(() => unscoped.handlers.authPasswordLogin(
      loginRequest(encodedKey(14).b64),
      handlerContext(),
    ));
    await expectNoDeviceWrites(unscoped.db);
  });

  test("runs an Argon2id-shaped dummy verification for unavailable credentials", async () => {
    const verifiedHashes: string[] = [];
    const verifyPassword = async (_password: string, hash: string): Promise<boolean> => {
      verifiedHashes.push(hash);
      return false;
    };
    const unavailable = [
      { email: "missing@example.test" },
      { status: "disabled" as const },
      { storedHash: null },
    ];
    for (const [index, accountOptions] of unavailable.entries()) {
      const harness = await createHarness({ verifyPassword });
      if (!("email" in accountOptions)) await provisionOwner(harness.db, accountOptions);
      await expectInvalidCredentials(() => harness.handlers.authPasswordLogin(
        loginRequest(encodedKey(40 + index).b64, {
          email: "email" in accountOptions ? accountOptions.email : EMAIL,
        }),
        handlerContext(),
      ));
    }
    expect(verifiedHashes).toHaveLength(3);
    expect(verifiedHashes.every((hash) => hash.startsWith("$argon2id$"))).toBe(true);
  });

  test("rejects an account with more than one active dashboard membership", async () => {
    const harness = await createHarness();
    await provisionOwner(harness.db);
    await harness.db.insertInto("dashboards").values({
      id: "dashboard-second",
      organization_id: ORGANIZATION_ID,
      slug: "second",
      name: "Second",
      status: "active",
      created_at_ms: NOW + 1,
    }).execute();
    await harness.db.insertInto("dashboard_memberships").values({
      dashboard_id: "dashboard-second",
      account_id: ACCOUNT_ID,
      role: "admin",
      created_at_ms: NOW + 1,
    }).execute();

    await expectInvalidCredentials(() => harness.handlers.authPasswordLogin(
      loginRequest(encodedKey(15).b64),
      handlerContext(),
    ));
    await expectNoDeviceWrites(harness.db);
  });

});
