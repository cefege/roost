// These fixtures own account topology and reset-link issuance for password-recovery tests.
// The password request and redemption suites call them to share identical database state.
// They depend on the recovery harness, bootstrap digests, and encrypted email outbox.
// Link assertions stay here because issuance is an observable contract shared by both callers.
import { expect } from "bun:test";
import { create } from "@bufbuild/protobuf";
import { AuthPasswordResetStartRequestSchema } from "@roost/shared/proto/coordinator_pb";
import { bootstrapTokenDigest } from "../src/bootstrap-tokens.ts";
import {
  ACCOUNT_ID,
  INSTANCE_ID,
  MANAGED_WEB_PUBLIC_ORIGIN,
  OWNER_EMAIL,
  TENANT_ROUTE_KEY,
  anonymousContext,
  captureConsole,
  type AccountRecoveryHarness,
} from "./account-recovery-harness.ts";

export async function seedRecoveryAccount(
  h: AccountRecoveryHarness,
  email = OWNER_EMAIL,
): Promise<{ fingerprints: string[]; otherFingerprint: string }> {
  const now = Date.now();
  const fingerprints = ["device-one", "device-two"];
  const otherFingerprint = "other-device";
  await h.db.insertInto("authorized_keys").values([
    { fingerprint: fingerprints[0]!, public_key: new Uint8Array(32), label: "one", added_at: now },
    { fingerprint: fingerprints[1]!, public_key: new Uint8Array(32), label: "two", added_at: now },
    { fingerprint: otherFingerprint, public_key: new Uint8Array(32), label: "other", added_at: now },
  ]).execute();
  await h.db.insertInto("accounts").values([
    {
      id: ACCOUNT_ID,
      email_normalized: email,
      password_hash: "old-password-hash",
      status: "active",
      created_at_ms: now,
      password_changed_at_ms: null,
    },
    {
      id: "other-account",
      email_normalized: "other@example.test",
      password_hash: "other-password-hash",
      status: "active",
      created_at_ms: now,
      password_changed_at_ms: null,
    },
  ]).execute();
  await h.db.insertInto("account_identities").values([
    {
      account_id: ACCOUNT_ID,
      issuer: "native",
      subject: ACCOUNT_ID,
      email_normalized: email,
      linked_at_ms: now,
      last_authenticated_at_ms: null,
      revoked_at_ms: null,
    },
    {
      account_id: "other-account",
      issuer: "native",
      subject: "other-account",
      email_normalized: "other@example.test",
      linked_at_ms: now,
      last_authenticated_at_ms: null,
      revoked_at_ms: null,
    },
  ]).execute();
  await h.db.insertInto("account_devices").values([
    { fingerprint: fingerprints[0]!, account_id: ACCOUNT_ID, added_at_ms: now, last_seen_at_ms: now },
    { fingerprint: fingerprints[1]!, account_id: ACCOUNT_ID, added_at_ms: now, last_seen_at_ms: now },
    { fingerprint: otherFingerprint, account_id: "other-account", added_at_ms: now, last_seen_at_ms: now },
  ]).execute();
  await h.db.insertInto("organizations").values({
    id: ACCOUNT_ID,
    slug: "personal",
    name: email,
    status: "active",
    created_at_ms: now,
  }).execute();
  await h.db.insertInto("organization_memberships").values({
    organization_id: ACCOUNT_ID,
    account_id: ACCOUNT_ID,
    role: "owner",
    created_at_ms: now,
  }).execute();
  await h.db.insertInto("dashboards").values({
    id: INSTANCE_ID,
    organization_id: ACCOUNT_ID,
    slug: "default",
    name: "Personal",
    status: "active",
    created_at_ms: now,
  }).execute();
  await h.db.insertInto("dashboard_memberships").values({
    dashboard_id: INSTANCE_ID,
    account_id: ACCOUNT_ID,
    role: "admin",
    created_at_ms: now,
  }).execute();
  await h.db.insertInto("push_subscriptions").values([
    {
      dashboard_id: INSTANCE_ID,
      viewer_fp: fingerprints[0]!,
      endpoint: "https://push.example.test/one",
      p256dh: "p256dh-one",
      auth: "auth-one",
      created_at_ms: now,
    },
    {
      dashboard_id: INSTANCE_ID,
      viewer_fp: fingerprints[1]!,
      endpoint: "https://push.example.test/two",
      p256dh: "p256dh-two",
      auth: "auth-two",
      created_at_ms: now,
    },
    {
      dashboard_id: INSTANCE_ID,
      viewer_fp: otherFingerprint,
      endpoint: "https://push.example.test/other",
      p256dh: "p256dh-other",
      auth: "auth-other",
      created_at_ms: now,
    },
  ]).execute();
  await h.db.insertInto("bootstrap_tokens").values([
    {
      token_hash: await bootstrapTokenDigest("unused-one"),
      account_id: ACCOUNT_ID,
      dashboard_id: INSTANCE_ID,
      kind: "browser",
      label: "unused one",
      created_at_ms: now,
      expires_at_ms: now + 60_000,
      used_at_ms: null,
      used_by_fp: null,
      minted_by_fp: fingerprints[0]!,
    },
    {
      token_hash: await bootstrapTokenDigest("unused-two"),
      account_id: ACCOUNT_ID,
      dashboard_id: INSTANCE_ID,
      kind: "worker",
      label: "unused two",
      created_at_ms: now,
      expires_at_ms: now + 60_000,
      used_at_ms: null,
      used_by_fp: null,
      minted_by_fp: fingerprints[1]!,
    },
    {
      token_hash: await bootstrapTokenDigest("already-used"),
      account_id: ACCOUNT_ID,
      dashboard_id: INSTANCE_ID,
      kind: "browser",
      label: "already used",
      created_at_ms: now,
      expires_at_ms: now + 60_000,
      used_at_ms: now,
      used_by_fp: fingerprints[0]!,
      minted_by_fp: fingerprints[0]!,
    },
    {
      token_hash: await bootstrapTokenDigest("other-unused"),
      account_id: "other-account",
      dashboard_id: INSTANCE_ID,
      kind: "browser",
      label: "other unused",
      created_at_ms: now,
      expires_at_ms: now + 60_000,
      used_at_ms: null,
      used_by_fp: null,
      minted_by_fp: otherFingerprint,
    },
  ]).execute();
  return { fingerprints, otherFingerprint };
}

export async function issueReset(
  h: AccountRecoveryHarness,
  email: string,
): Promise<{ token: string; tokenHash: string; output: string }> {
  const issued = await captureConsole(() => h.handlers.authPasswordResetRequest(
    create(AuthPasswordResetStartRequestSchema, { email }),
    anonymousContext(),
  ));
  const outbox = await h.db.selectFrom("email_outbox")
    .selectAll()
    .where("kind", "=", "password_reset")
    .orderBy("next_attempt_ms", "desc")
    .executeTakeFirstOrThrow();
  const payload = h.cipher.decrypt(
    { outboxId: outbox.id, kind: outbox.kind },
    outbox.encrypted_payload,
  );
  const match = payload.text?.match(/https:\/\/\S+$/)?.[0];
  if (!match) throw new Error("missing reset link");
  const link = new URL(match);
  expect(link.origin).toBe(MANAGED_WEB_PUBLIC_ORIGIN);
  expect(link.pathname).toBe(`/reset-password/${TENANT_ROUTE_KEY}`);
  expect(link.search).toBe("");
  const token = link.hash.slice(1);
  if (!token) throw new Error("missing reset fragment token");
  expect(link.hash).toBe(`#${token}`);
  expect(match).toBe(
    `${MANAGED_WEB_PUBLIC_ORIGIN}/reset-password/${TENANT_ROUTE_KEY}#${token}`,
  );
  expect(link.hash).not.toContain("token=");
  expect(match.slice(0, match.indexOf("#"))).not.toContain(token);
  const reset = await h.db.selectFrom("password_reset_tokens")
    .select("token_hash")
    .where("account_id", "=", ACCOUNT_ID)
    .where("used_at_ms", "is", null)
    .executeTakeFirstOrThrow();
  return { token, tokenHash: reset.token_hash, output: issued.output };
}
