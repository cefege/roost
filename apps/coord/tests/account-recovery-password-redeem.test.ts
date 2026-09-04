// This suite owns password-reset redemption, revocation, and invalid-token behavior.
// Bun discovers it directly and invokes account handlers through isolated recovery databases.
// It depends on shared password fixtures for the complete credential topology and reset token.
// Per-file cleanup closes every temporary database after its test finishes.
import { afterEach, describe, expect, test } from "bun:test";
import { create } from "@bufbuild/protobuf";
import { AuthPasswordResetRedeemRequestSchema } from "@roost/shared/proto/coordinator_pb";
import { bootstrapTokenDigest } from "../src/bootstrap-tokens.ts";
import {
  ACCOUNT_ID,
  INSTANCE_ID,
  OWNER_EMAIL,
  anonymousContext,
  captureConsole,
  createAccountRecoveryHarnessScope,
  expectGenericDenial,
  expectInvalidPassword,
  tokenHash,
} from "./account-recovery-harness.ts";
import {
  issueReset,
  seedRecoveryAccount,
} from "./account-recovery-password-fixtures.ts";

const harnessScope = createAccountRecoveryHarnessScope();
const harness = harnessScope.open;
afterEach(harnessScope.closeAll);

describe("password recovery", () => {
  test("atomically replaces the password and revokes every old browser credential", async () => {
    const h = await harness();
    const { fingerprints, otherFingerprint } = await seedRecoveryAccount(h);
    const issued = await issueReset(h, OWNER_EMAIL);
    const password = "a secure replacement password";
    const redeemed = await captureConsole(() => h.handlers.authPasswordResetRedeem(
      create(AuthPasswordResetRedeemRequestSchema, {
        token: issued.token,
        newPassword: password,
      }),
      anonymousContext(),
    ));

    expect(redeemed.result.ok).toBe(true);
    expect((await h.db.selectFrom("accounts")
      .select("password_hash")
      .where("id", "=", ACCOUNT_ID)
      .executeTakeFirstOrThrow()).password_hash).toBe(`test-hash:${tokenHash(password)}`);
    expect(await h.db.selectFrom("account_devices")
      .select("fingerprint")
      .where("account_id", "=", ACCOUNT_ID)
      .execute()).toEqual([]);
    expect(await h.db.selectFrom("authorized_keys")
      .select("fingerprint")
      .orderBy("fingerprint")
      .execute()).toEqual([{ fingerprint: otherFingerprint }]);
    expect(await h.db.selectFrom("authorized_key_revocations")
      .select("fingerprint")
      .orderBy("fingerprint")
      .execute()).toEqual(fingerprints.slice().sort().map((fingerprint) => ({ fingerprint })));
    expect(await h.db.selectFrom("push_subscriptions")
      .select("viewer_fp")
      .execute()).toEqual([{ viewer_fp: otherFingerprint }]);
    const remainingBootstrapTokenHashes = [
      await bootstrapTokenDigest("already-used"),
      await bootstrapTokenDigest("other-unused"),
    ].sort();
    expect(await h.db.selectFrom("bootstrap_tokens")
      .select("token_hash")
      .orderBy("token_hash")
      .execute()).toEqual(
      remainingBootstrapTokenHashes.map((token_hash) => ({ token_hash })),
    );
    expect((await h.db.selectFrom("password_reset_tokens")
      .select("used_at_ms")
      .where("token_hash", "=", issued.tokenHash)
      .executeTakeFirstOrThrow()).used_at_ms).not.toBeNull();
    expect(h.revoked.slice().sort()).toEqual(fingerprints.slice().sort());
    expect(h.dashboardRevocations).toHaveLength(fingerprints.length);
    expect(h.dashboardRevocations.every(({ dashboardId }) => dashboardId === INSTANCE_ID)).toBe(true);
    expect(h.callbacksSawCommittedState.every(Boolean)).toBe(true);
    expect(redeemed.output).not.toContain(OWNER_EMAIL);
    expect(redeemed.output).not.toContain(issued.token);
    expect(redeemed.output).not.toContain(password);
    await expectGenericDenial(() => h.handlers.authPasswordResetRedeem(
      create(AuthPasswordResetRedeemRequestSchema, {
        token: issued.token,
        newPassword: "another secure replacement password",
      }),
      anonymousContext(),
    ));
  });

  test("enforces the 12 through 1024 character reset setter boundary", async () => {
    for (const [length, valid] of [[11, false], [12, true], [1_024, true], [1_025, false]] as const) {
      const h = await harness();
      const email = `owner-${length}@example.test`;
      await seedRecoveryAccount(h, email);
      const issued = await issueReset(h, email);
      const password = "r".repeat(length);
      if (valid) {
        const response = await h.handlers.authPasswordResetRedeem(
          create(AuthPasswordResetRedeemRequestSchema, {
            token: issued.token,
            newPassword: password,
          }),
          anonymousContext(),
        );
        expect(response.ok).toBe(true);
        expect(h.passwordGate.calls).toEqual([password]);
      } else {
        await expectInvalidPassword(() => h.handlers.authPasswordResetRedeem(
          create(AuthPasswordResetRedeemRequestSchema, {
            token: issued.token,
            newPassword: password,
          }),
          anonymousContext(),
        ));
        expect(h.passwordGate.calls).toEqual([]);
        expect((await h.db.selectFrom("password_reset_tokens")
          .select("used_at_ms")
          .where("token_hash", "=", issued.tokenHash)
          .executeTakeFirstOrThrow()).used_at_ms).toBeNull();
      }
      await h.close();
    }
  });

  test("denies expired reset tokens without changing password or devices", async () => {
    const h = await harness();
    const { fingerprints } = await seedRecoveryAccount(h, "expired@example.test");
    const issued = await issueReset(h, "expired@example.test");
    await h.db.updateTable("password_reset_tokens")
      .set({ expires_at_ms: Date.now() - 1 })
      .where("token_hash", "=", issued.tokenHash)
      .execute();

    await expectGenericDenial(() => h.handlers.authPasswordResetRedeem(
      create(AuthPasswordResetRedeemRequestSchema, {
        token: issued.token,
        newPassword: "a secure replacement password",
      }),
      anonymousContext(),
    ));
    expect((await h.db.selectFrom("accounts")
      .select("password_hash")
      .where("id", "=", ACCOUNT_ID)
      .executeTakeFirstOrThrow()).password_hash).toBe("old-password-hash");
    expect(await h.db.selectFrom("account_devices")
      .select("fingerprint")
      .where("account_id", "=", ACCOUNT_ID)
      .orderBy("fingerprint")
      .execute()).toEqual(fingerprints.slice().sort().map((fingerprint) => ({ fingerprint })));
  });
});
