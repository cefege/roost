// This suite owns owner-activation behavior for managed coordinator accounts.
// Bun discovers it directly and exercises the account activation handler as its caller.
// It depends on the recovery harness for real migrations, handler wiring, and cleanup.
// Activation tokens and device-key setup stay local because no other suite consumes them.
import { afterEach, describe, expect, test } from "bun:test";
import { create } from "@bufbuild/protobuf";
import type { HandlerContext } from "@connectrpc/connect";
import { fingerprintOf } from "@roost/shared/fingerprint";
import { AuthOwnerActivateRequestSchema } from "@roost/shared/proto/coordinator_pb";
import {
  ACCOUNT_ID,
  INSTANCE_ID,
  OWNER_EMAIL,
  anonymousContext,
  authenticatedContext,
  captureConsole,
  createAccountRecoveryHarnessScope,
  expectGenericDenial,
  expectInvalidPassword,
  tokenHash,
  type AccountRecoveryHarness as Harness,
} from "./account-recovery-harness.ts";

const ACTIVATION_TOKEN = Buffer.alloc(32, 7).toString("base64url");
const DEVICE_KEY = Uint8Array.from({ length: 32 }, (_, index) => index + 1);
const DEVICE_KEY_B64 = Buffer.from(DEVICE_KEY).toString("base64url");

const harnessScope = createAccountRecoveryHarnessScope();
const harness = harnessScope.open;
afterEach(harnessScope.closeAll);

async function seedActivation(h: Harness, options: {
  token?: string;
  coordinatorId?: string;
  accountId?: string;
  email?: string;
  expiresAtMs?: number;
  revokedAtMs?: number | null;
} = {}): Promise<string> {
  const token = options.token ?? ACTIVATION_TOKEN;
  const now = Date.now();
  const outboxId = `activation-outbox-${tokenHash(token).slice(0, 12)}`;
  const email = options.email ?? OWNER_EMAIL;
  const encryptedPayload = h.cipher.encrypt(
    { outboxId, kind: "owner_activation" },
    {
      subject: "Activate your Roost account",
      html: "<p>Activate your Roost account.</p>",
      text: "Activate your Roost account.",
    },
  );
  await h.db.insertInto("email_outbox").values({
    id: outboxId,
    kind: "owner_activation",
    recipient: email,
    encrypted_payload: encryptedPayload,
    idempotency_key: outboxId,
    state: "pending",
    attempts: 0,
    locked_until_ms: null,
    lease_token: null,
    next_attempt_ms: Number.MAX_SAFE_INTEGER,
    provider_message_id: null,
    sent_at_ms: null,
    failed_at_ms: null,
    last_error: null,
  }).execute();
  await h.db.insertInto("owner_activation_tokens").values({
    coordinator_id: options.coordinatorId ?? INSTANCE_ID,
    account_id: options.accountId ?? ACCOUNT_ID,
    email_normalized: email,
    token_hash: tokenHash(token),
    outbox_id: outboxId,
    delivery: "coordinator-email",
    created_at_ms: now,
    expires_at_ms: options.expiresAtMs ?? now + 60_000,
    accepted_at_ms: null,
    revoked_at_ms: options.revokedAtMs ?? null,
  }).execute();
  return token;
}

function activationRequest(token: string, password: string) {
  return create(AuthOwnerActivateRequestSchema, {
    token,
    newPassword: password,
    sshPubkeyB64: DEVICE_KEY_B64,
    label: "  Owner browser  ",
  });
}

describe("owner activation", () => {
  test("atomically creates the fixed single-owner topology and authorizes only after commit", async () => {
    const h = await harness();
    const token = await seedActivation(h);
    const password = "a secure owner password";
    const activated = await captureConsole(() => h.handlers.authOwnerActivate(
      activationRequest(token, password),
      anonymousContext(),
    ));
    const fingerprint = await fingerprintOf(DEVICE_KEY);

    expect(activated.result.dashboardId).toBe(INSTANCE_ID);
    expect(h.passwordGate.calls).toEqual([password]);
    expect(await h.db.selectFrom("accounts").selectAll().execute()).toEqual([
      expect.objectContaining({
        id: ACCOUNT_ID,
        email_normalized: OWNER_EMAIL,
        status: "active",
      }),
    ]);
    const account = await h.db.selectFrom("accounts")
      .select(["password_hash", "password_changed_at_ms"])
      .executeTakeFirstOrThrow();
    expect(account.password_hash).toBe(`test-hash:${tokenHash(password)}`);
    expect(account.password_hash).not.toContain(password);
    expect(account.password_changed_at_ms).not.toBeNull();
    expect(await h.db.selectFrom("account_identities").selectAll().execute()).toEqual([{
      account_id: ACCOUNT_ID,
      issuer: "native",
      subject: ACCOUNT_ID,
      email_normalized: OWNER_EMAIL,
      linked_at_ms: expect.any(Number),
      last_authenticated_at_ms: null,
      revoked_at_ms: null,
    }]);
    expect(await h.db.selectFrom("organizations")
      .select(["id", "slug", "name", "status"])
      .execute()).toEqual([{
      id: ACCOUNT_ID,
      slug: "personal",
      name: OWNER_EMAIL,
      status: "active",
    }]);
    expect(await h.db.selectFrom("organization_memberships")
      .select(["organization_id", "account_id", "role"])
      .execute()).toEqual([{
      organization_id: ACCOUNT_ID,
      account_id: ACCOUNT_ID,
      role: "owner",
    }]);
    expect(await h.db.selectFrom("dashboards")
      .select(["id", "organization_id", "slug", "name", "status"])
      .execute()).toEqual([{
      id: INSTANCE_ID,
      organization_id: ACCOUNT_ID,
      slug: "default",
      name: "Personal",
      status: "active",
    }]);
    expect(await h.db.selectFrom("dashboard_memberships")
      .select(["dashboard_id", "account_id", "role"])
      .execute()).toEqual([{
      dashboard_id: INSTANCE_ID,
      account_id: ACCOUNT_ID,
      role: "admin",
    }]);
    expect(await h.db.selectFrom("authorized_keys")
      .select(["fingerprint", "label"])
      .execute()).toEqual([{ fingerprint, label: "Owner browser" }]);
    expect(await h.db.selectFrom("account_devices")
      .select(["fingerprint", "account_id"])
      .execute()).toEqual([{ fingerprint, account_id: ACCOUNT_ID }]);
    expect((await h.db.selectFrom("owner_activation_tokens")
      .select("accepted_at_ms")
      .executeTakeFirstOrThrow()).accepted_at_ms).not.toBeNull();
    expect(activated.output).not.toContain(token);
    expect(activated.output).not.toContain(OWNER_EMAIL);
    expect(activated.output).not.toContain(password);

    await expectGenericDenial(() => h.handlers.authOwnerActivate(
      activationRequest(token, "another secure password"),
      anonymousContext(),
    ));
  });

  test("makes expired, revoked, foreign, conflicting, and authenticated claims indistinguishable", async () => {
    const cases: Array<{
      name: string;
      seed?: Parameters<typeof seedActivation>[1];
      context?: HandlerContext;
      prepare?: (h: Harness) => Promise<void>;
    }> = [
      { name: "expired", seed: { expiresAtMs: Date.now() - 1 } },
      { name: "revoked", seed: { revokedAtMs: Date.now() } },
      { name: "foreign", seed: { coordinatorId: "foreign-coordinator" } },
      {
        name: "key collision",
        prepare: async (h) => {
          const fingerprint = await fingerprintOf(DEVICE_KEY);
          await h.db.insertInto("authorized_keys").values({
            fingerprint,
            public_key: DEVICE_KEY,
            label: "collision",
            added_at: Date.now(),
          }).execute();
        },
      },
      {
        name: "worker collision",
        prepare: async (h) => {
          const fingerprint = await fingerprintOf(DEVICE_KEY);
          h.sqlite.exec("PRAGMA foreign_keys = OFF");
          try {
            await h.db.insertInto("workers").values({
              fp: fingerprint,
              dashboard_id: INSTANCE_ID,
              label: "collision",
              os: "linux",
              git_sha: null,
              host_metrics_json: null,
              registered_at_ms: Date.now(),
              last_seen_ms: Date.now(),
              reachable_addr: null,
              keeper_stale: null,
            }).execute();
          } finally {
            h.sqlite.exec("PRAGMA foreign_keys = ON");
          }
        },
      },
      {
        name: "already activated",
        prepare: async (h) => {
          await h.db.insertInto("accounts").values({
            id: "existing-account",
            email_normalized: "existing@example.test",
            password_hash: "existing-hash",
            status: "active",
            created_at_ms: Date.now(),
            password_changed_at_ms: Date.now(),
          }).execute();
        },
      },
      { name: "authenticated caller", context: authenticatedContext() },
    ];

    for (const scenario of cases) {
      const h = await harness();
      const token = await seedActivation(h, scenario.seed);
      await scenario.prepare?.(h);
      await expectGenericDenial(() => h.handlers.authOwnerActivate(
        activationRequest(token, "a secure owner password"),
        scenario.context ?? anonymousContext(),
      ));
      expect((await h.db.selectFrom("owner_activation_tokens")
        .select("accepted_at_ms")
        .executeTakeFirstOrThrow()).accepted_at_ms, scenario.name).toBeNull();
      expect(await h.db.selectFrom("organization_memberships").selectAll().execute(), scenario.name)
        .toEqual([]);
      await h.close();
    }

    const unmanaged = await harness({ managedContainer: false });
    const token = await seedActivation(unmanaged);
    await expectGenericDenial(() => unmanaged.handlers.authOwnerActivate(
      activationRequest(token, "a secure owner password"),
      anonymousContext(),
    ));
  });

  test("enforces the 12 through 1024 character setter boundary", async () => {
    for (const [length, valid] of [[11, false], [12, true], [1_024, true], [1_025, false]] as const) {
      const h = await harness();
      const token = await seedActivation(h);
      const password = "p".repeat(length);
      if (valid) {
        const response = await h.handlers.authOwnerActivate(
          activationRequest(token, password),
          anonymousContext(),
        );
        expect(response.dashboardId).toBe(INSTANCE_ID);
        expect(h.passwordGate.calls).toEqual([password]);
      } else {
        await expectInvalidPassword(() => h.handlers.authOwnerActivate(
          activationRequest(token, password),
          anonymousContext(),
        ));
        expect(h.passwordGate.calls).toEqual([]);
        expect((await h.db.selectFrom("owner_activation_tokens")
          .select("accepted_at_ms")
          .executeTakeFirstOrThrow()).accepted_at_ms).toBeNull();
      }
      await h.close();
    }
  });

  test("rolls back token consumption and every owner row when a topology insert fails", async () => {
    const h = await harness();
    const token = await seedActivation(h);
    h.sqlite.exec(`
      CREATE TRIGGER fail_owner_dashboard
      BEFORE INSERT ON dashboards
      BEGIN
        SELECT RAISE(ABORT, 'forced activation rollback');
      END
    `);

    await expect(h.handlers.authOwnerActivate(
      activationRequest(token, "a secure owner password"),
      anonymousContext(),
    )).rejects.toThrow("forced activation rollback");
    for (const table of [
      "accounts",
      "account_identities",
      "organizations",
      "organization_memberships",
      "dashboards",
      "dashboard_memberships",
      "authorized_keys",
      "account_devices",
    ]) {
      expect(h.sqlite.query(`SELECT COUNT(*) AS count FROM ${table}`).get(), table)
        .toEqual({ count: 0 });
    }
    expect(await h.db.selectFrom("owner_activation_tokens")
      .select("accepted_at_ms")
      .executeTakeFirst()).toEqual({ accepted_at_ms: null });
  });
});
