// This suite owns password-reset acknowledgement and credential-delivery behavior.
// Bun discovers it directly and invokes account handlers through isolated recovery databases.
// It depends on the shared password fixtures for seeded accounts and decoded reset links.
// Per-file cleanup closes every temporary database after its test finishes.
import { afterEach, describe, expect, test } from "bun:test";
import { create } from "@bufbuild/protobuf";
import { AuthPasswordResetStartRequestSchema } from "@roost/shared/proto/coordinator_pb";
import {
  ACCOUNT_ID,
  OWNER_EMAIL,
  anonymousContext,
  createAccountRecoveryHarnessScope,
} from "./account-recovery-harness.ts";
import {
  issueReset,
  seedRecoveryAccount,
} from "./account-recovery-password-fixtures.ts";

const harnessScope = createAccountRecoveryHarnessScope();
const harness = harnessScope.open;
afterEach(harnessScope.closeAll);

describe("password recovery", () => {
  test("keeps reset acknowledgement uniform and puts the credential only in the tenant-scoped fragment", async () => {
    const h = await harness();
    await seedRecoveryAccount(h);
    const unknown = await h.handlers.authPasswordResetRequest(
      create(AuthPasswordResetStartRequestSchema, { email: "unknown@example.test" }),
      anonymousContext(),
    );
    expect(unknown).toBeDefined();
    expect(await h.db.selectFrom("password_reset_tokens").selectAll().execute()).toEqual([]);

    const issued = await issueReset(h, " Owner@Example.TEST ");
    expect(issued.token).toHaveLength(43);
    expect(issued.tokenHash).toHaveLength(64);
    expect(issued.tokenHash).not.toBe(issued.token);
    expect(issued.output).not.toContain(OWNER_EMAIL);
    expect(issued.output).not.toContain(issued.token);
  });

  test("acknowledges a Google-only passwordless account without issuing reset state or email", async () => {
    const h = await harness();
    await seedRecoveryAccount(h);
    await h.db.updateTable("accounts")
      .set({ password_hash: null, password_changed_at_ms: null })
      .where("id", "=", ACCOUNT_ID)
      .execute();
    await h.db.deleteFrom("account_identities")
      .where("issuer", "=", "native")
      .where("subject", "=", ACCOUNT_ID)
      .execute();
    await h.db.insertInto("account_identities").values({
      account_id: ACCOUNT_ID,
      issuer: "https://accounts.google.com",
      subject: "google-passwordless-owner",
      email_normalized: OWNER_EMAIL,
      linked_at_ms: Date.now(),
      last_authenticated_at_ms: null,
      revoked_at_ms: null,
    }).execute();

    const response = await h.handlers.authPasswordResetRequest(
      create(AuthPasswordResetStartRequestSchema, { email: OWNER_EMAIL }),
      anonymousContext(),
    );
    expect(response).toBeDefined();
    expect(await h.db.selectFrom("password_reset_tokens").selectAll().execute()).toEqual([]);
    expect(await h.db.selectFrom("email_outbox").selectAll()
      .where("kind", "=", "password_reset").execute()).toEqual([]);
  });
});
