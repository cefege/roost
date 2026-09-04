/**
 * This suite pins the transactional owner activation and managed topology invariants.
 * Keeping database-heavy cases together makes their shared migration lifecycle explicit.
 * Shared row readers preserve the original assertions while this file owns cleanup.
 */
import { afterEach, describe, expect, test } from "bun:test";
import {
  parseGoogleOwnerSeedPayload,
  parseSignupGatewayActivationHash,
  type OwnerActivationStatus,
  readOwnerActivationStatus,
  releaseOwnerActivationEmail,
  seedGoogleOwner,
  seedOwnerActivation,
  seedSignupGatewayOwnerActivation,
} from "../src/saas-instance.ts";
import {
  ACCOUNT_ID,
  COORDINATOR_ID,
  EMAIL,
  OUTBOX_KEY,
  PUBLIC_URL,
  ROUTE_KEY,
  activation,
  cleanupSaasInstanceRoots,
  createMigratedDatabase,
  decryptedToken,
  hashToken,
  outbox,
  rowCount,
} from "./saas-instance-fixtures.ts";

const roots: string[] = [];

async function migratedDatabase() {
  return createMigratedDatabase(roots);
}

afterEach(async () => {
  await cleanupSaasInstanceRoots(roots);
});

describe("owner activation instance state", () => {
  test("atomically holds, rolls back, reseeds, invalidates, and idempotently releases one encrypted link", async () => {
    const { sqlite } = await migratedDatabase();
    try {
      seedOwnerActivation(sqlite, {
        accountId: ACCOUNT_ID,
        coordinatorId: COORDINATOR_ID,
        email: " Owner@Example.COM ",
      }, { emailOutboxKey: OUTBOX_KEY, webPublicUrl: PUBLIC_URL, tenantRouteKey: ROUTE_KEY, now: () => 1_000,
      createId: () => "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
      createTokenBytes: () => Buffer.alloc(32, 1), });

      const firstActivation = activation(sqlite);
      const firstOutbox = outbox(sqlite);
      const firstSecret = decryptedToken(firstOutbox);
      expect(firstSecret.link).toBe(
        `${PUBLIC_URL}/activate/${ROUTE_KEY}#${firstSecret.token}`,
      );
      expect(firstSecret.link).not.toContain("?");
      expect(firstSecret.token).toHaveLength(43);
      expect(firstActivation).toMatchObject({
        coordinator_id: COORDINATOR_ID,
        account_id: ACCOUNT_ID,
        email_normalized: EMAIL,
        token_hash: hashToken(firstSecret.token),
        outbox_id: firstOutbox.id,
        created_at_ms: 1_000,
        accepted_at_ms: null,
        revoked_at_ms: null,
      });
      expect(firstActivation.expires_at_ms - firstActivation.created_at_ms).toBe(7 * 24 * 60 * 60 * 1_000);
      expect(firstOutbox).toMatchObject({
        kind: "owner_activation",
        recipient: EMAIL,
        state: "pending",
        attempts: 0,
        next_attempt_ms: Number.MAX_SAFE_INTEGER,
      });
      expect(firstOutbox.encrypted_payload).not.toContain(firstSecret.token);
      expect(firstOutbox.encrypted_payload).not.toContain(firstSecret.link);
      expect(rowCount(sqlite, "owner_activation_tokens")).toBe(1);
      expect(rowCount(sqlite, "email_outbox")).toBe(1);

      sqlite.exec(`CREATE TRIGGER reject_owner_activation_seed
        BEFORE INSERT ON owner_activation_tokens
        BEGIN SELECT RAISE(ABORT, 'injected seed failure'); END`);
      expect(() => seedOwnerActivation(sqlite, {
        accountId: ACCOUNT_ID,
        coordinatorId: COORDINATOR_ID,
        email: EMAIL,
      }, { emailOutboxKey: OUTBOX_KEY, webPublicUrl: PUBLIC_URL, tenantRouteKey: ROUTE_KEY, now: () => 2_000,
      createId: () => "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
      createTokenBytes: () => Buffer.alloc(32, 2), })).toThrow("injected seed failure");
      expect(activation(sqlite)).toEqual(firstActivation);
      expect(outbox(sqlite)).toEqual(firstOutbox);
      sqlite.exec("DROP TRIGGER reject_owner_activation_seed");

      seedOwnerActivation(sqlite, {
        accountId: ACCOUNT_ID,
        coordinatorId: COORDINATOR_ID,
        email: EMAIL,
      }, { emailOutboxKey: OUTBOX_KEY, webPublicUrl: PUBLIC_URL, tenantRouteKey: ROUTE_KEY, now: () => 2_000,
      createId: () => "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
      createTokenBytes: () => Buffer.alloc(32, 2), });
      const secondActivation = activation(sqlite);
      const secondOutbox = outbox(sqlite);
      const secondSecret = decryptedToken(secondOutbox);
      expect(secondActivation.token_hash).toBe(hashToken(secondSecret.token));
      expect(secondActivation.token_hash).not.toBe(firstActivation.token_hash);
      expect(secondOutbox.id).not.toBe(firstOutbox.id);
      expect(sqlite.query("SELECT 1 FROM owner_activation_tokens WHERE token_hash = ?").get(
        firstActivation.token_hash,
      )).toBeNull();
      expect(sqlite.query("SELECT 1 FROM email_outbox WHERE id = ?").get(firstOutbox.id)).toBeNull();
      expect(rowCount(sqlite, "owner_activation_tokens")).toBe(1);
      expect(rowCount(sqlite, "email_outbox")).toBe(1);
      expect(secondOutbox.next_attempt_ms).toBe(Number.MAX_SAFE_INTEGER);
      expect(() => releaseOwnerActivationEmail(
        sqlite,
        () => 3_000,
        "33333333-3333-4333-8333-333333333333",
      )).toThrow("does not match");
      expect(outbox(sqlite).next_attempt_ms).toBe(Number.MAX_SAFE_INTEGER);


      expect(releaseOwnerActivationEmail(sqlite, () => 3_000)).toEqual({
        accountId: ACCOUNT_ID,
        coordinatorId: COORDINATOR_ID,
      });
      expect(outbox(sqlite).next_attempt_ms).toBe(3_000);
      expect(releaseOwnerActivationEmail(sqlite, () => 4_000)).toEqual({
        accountId: ACCOUNT_ID,
        coordinatorId: COORDINATOR_ID,
      });
      expect(outbox(sqlite).next_attempt_ms).toBe(3_000);

      sqlite.query(
        `INSERT INTO accounts
          (id, email_normalized, password_hash, status, created_at_ms, password_changed_at_ms)
         VALUES (?, ?, NULL, 'active', 5_000, NULL)`,
      ).run(ACCOUNT_ID, EMAIL);
      expect(() => seedOwnerActivation(sqlite, {
        accountId: ACCOUNT_ID,
        coordinatorId: COORDINATOR_ID,
        email: EMAIL,
      }, { emailOutboxKey: OUTBOX_KEY, webPublicUrl: PUBLIC_URL, tenantRouteKey: ROUTE_KEY, now: () => 5_000, })).toThrow("account already exists");
      expect(activation(sqlite)).toEqual(secondActivation);
      expect(outbox(sqlite).id).toBe(secondOutbox.id);
    } finally {
      sqlite.close(true);
    }
  });

  test("status is idempotent and proves the complete committed owner transaction", async () => {
    const { sqlite } = await migratedDatabase();
    try {
      seedOwnerActivation(sqlite, {
        accountId: ACCOUNT_ID,
        coordinatorId: COORDINATOR_ID,
        email: EMAIL,
      }, { emailOutboxKey: OUTBOX_KEY, webPublicUrl: PUBLIC_URL, tenantRouteKey: ROUTE_KEY, now: () => 10_000,
      createId: () => "cccccccc-cccc-4ccc-8ccc-cccccccccccc",
      createTokenBytes: () => Buffer.alloc(32, 3), });
      const pending: OwnerActivationStatus = {
        accountId: ACCOUNT_ID,
        coordinatorId: COORDINATOR_ID,
        activated: false,
        expiresAtMs: 604_810_000,
        topology: "pending-coordinator-email",
      };
      expect(readOwnerActivationStatus(sqlite, COORDINATOR_ID, 10_000)).toEqual(pending);
      expect(readOwnerActivationStatus(sqlite, COORDINATOR_ID, 10_000)).toEqual(pending);

      sqlite.exec("BEGIN IMMEDIATE");
      sqlite.query(
        "INSERT INTO authorized_keys (fingerprint, public_key, label, added_at) VALUES ('owner-device', ?, 'Owner browser', 11_000)",
      ).run(Buffer.alloc(32, 9));
      sqlite.query(
        `INSERT INTO accounts
          (id, email_normalized, password_hash, status, created_at_ms, password_changed_at_ms)
         VALUES (?, ?, 'argon2id-fixture', 'active', 11_000, 11_000)`,
      ).run(ACCOUNT_ID, EMAIL);
      sqlite.query(
        `INSERT INTO account_identities (
           account_id, issuer, subject, email_normalized, linked_at_ms,
           last_authenticated_at_ms, revoked_at_ms
         ) VALUES (?, 'native', ?, ?, 11_000, NULL, NULL)`,
      ).run(ACCOUNT_ID, ACCOUNT_ID, EMAIL);
      sqlite.query(
        "INSERT INTO organizations (id, slug, name, status, created_at_ms) VALUES (?, 'personal', ?, 'active', 11_000)",
      ).run(ACCOUNT_ID, EMAIL);
      sqlite.query(
        "INSERT INTO organization_memberships (organization_id, account_id, role, created_at_ms) VALUES (?, ?, 'owner', 11_000)",
      ).run(ACCOUNT_ID, ACCOUNT_ID);
      sqlite.query(
        `INSERT INTO dashboards (id, organization_id, slug, name, status, created_at_ms)
         VALUES (?, ?, 'default', 'Personal', 'active', 11_000)`,
      ).run(COORDINATOR_ID, ACCOUNT_ID);
      sqlite.query(
        "INSERT INTO dashboard_memberships (dashboard_id, account_id, role, created_at_ms) VALUES (?, ?, 'admin', 11_000)",
      ).run(COORDINATOR_ID, ACCOUNT_ID);
      sqlite.query(
        `INSERT INTO account_devices (fingerprint, account_id, added_at_ms, last_seen_at_ms)
         VALUES ('owner-device', ?, 11_000, 11_000)`,
      ).run(ACCOUNT_ID);
      sqlite.query(
        "UPDATE owner_activation_tokens SET accepted_at_ms = 11_000 WHERE coordinator_id = ?",
      ).run(COORDINATOR_ID);
      sqlite.exec("COMMIT");

      const activated: OwnerActivationStatus = {
        accountId: ACCOUNT_ID,
        coordinatorId: COORDINATOR_ID,
        activated: true,
        expiresAtMs: 604_810_000,
        topology: "active-native-password",
      };
      expect(readOwnerActivationStatus(sqlite, COORDINATOR_ID, 11_000)).toEqual(activated);
      expect(readOwnerActivationStatus(sqlite, COORDINATOR_ID, 11_000)).toEqual(activated);
      sqlite.query("UPDATE dashboard_memberships SET role = 'member'").run();
      expect(() => readOwnerActivationStatus(sqlite, COORDINATOR_ID, 11_000))
        .toThrow("managed-container invariant violation");
    } finally {
      sqlite.close(true);
    }
  });
  test("seeds exact signup-gateway and passwordless Google owner topologies", async () => {
    const external = await migratedDatabase();
    try {
      const hash = "d".repeat(64);
      seedSignupGatewayOwnerActivation(external.sqlite, {
        accountId: ACCOUNT_ID,
        coordinatorId: COORDINATOR_ID,
        email: EMAIL,
      }, hash, { now: () => 20_000 });
      seedSignupGatewayOwnerActivation(external.sqlite, {
        accountId: ACCOUNT_ID,
        coordinatorId: COORDINATOR_ID,
        email: EMAIL,
      }, hash, { now: () => 30_000 });
      expect(external.sqlite.query<{
        token_hash: string;
        outbox_id: string | null;
        delivery: string;
        created_at_ms: number;
      }, []>(`
        SELECT token_hash, outbox_id, delivery, created_at_ms
        FROM owner_activation_tokens
      `).get()).toEqual({
        token_hash: hash,
        outbox_id: null,
        delivery: "signup-gateway",
        created_at_ms: 20_000,
      });
      expect(rowCount(external.sqlite, "email_outbox")).toBe(0);
      expect(() => seedSignupGatewayOwnerActivation(external.sqlite, {
        accountId: ACCOUNT_ID,
        coordinatorId: COORDINATOR_ID,
        email: EMAIL,
      }, "e".repeat(64))).toThrow("topology differs");
    } finally {
      external.sqlite.close(true);
    }

    const google = await migratedDatabase();
    try {
      const payload = { subject: "google-subject-123", emailNormalized: EMAIL };
      seedGoogleOwner(google.sqlite, {
        accountId: ACCOUNT_ID,
        coordinatorId: COORDINATOR_ID,
      }, payload, { now: () => 40_000 });
      seedGoogleOwner(google.sqlite, {
        accountId: ACCOUNT_ID,
        coordinatorId: COORDINATOR_ID,
      }, payload, { now: () => 50_000 });
      expect(google.sqlite.query(`
        SELECT password_hash, password_changed_at_ms, status FROM accounts
      `).get()).toEqual({
        password_hash: null,
        password_changed_at_ms: null,
        status: "active",
      });
      expect(google.sqlite.query(`
        SELECT issuer, subject, email_normalized, linked_at_ms,
               last_authenticated_at_ms, revoked_at_ms
        FROM account_identities
      `).get()).toEqual({
        issuer: "https://accounts.google.com",
        subject: payload.subject,
        email_normalized: EMAIL,
        linked_at_ms: 40_000,
        last_authenticated_at_ms: 40_000,
        revoked_at_ms: null,
      });
      expect(rowCount(google.sqlite, "account_devices")).toBe(0);
      expect(rowCount(google.sqlite, "email_outbox")).toBe(0);
      expect(() => seedGoogleOwner(google.sqlite, {
        accountId: ACCOUNT_ID,
        coordinatorId: COORDINATOR_ID,
      }, { ...payload, subject: "different-subject" })).toThrow("topology differs");
    } finally {
      google.sqlite.close(true);
    }
  });

  test("accepts only bounded canonical sensitive stdin payloads", () => {
    const hash = "f".repeat(64);
    expect(parseSignupGatewayActivationHash(Buffer.from(`${hash}\n`))).toBe(hash);
    expect(parseGoogleOwnerSeedPayload(Buffer.from(
      `${JSON.stringify({ subject: "123456", emailNormalized: EMAIL })}\n`,
    ))).toEqual({ subject: "123456", emailNormalized: EMAIL });
    for (const invalid of [
      Buffer.from("F".repeat(64)),
      Buffer.from(`${hash}\n\n`),
      Buffer.alloc(1_025, 97),
    ]) {
      expect(() => parseSignupGatewayActivationHash(invalid)).toThrow();
    }
    expect(() => parseGoogleOwnerSeedPayload(Buffer.from(
      JSON.stringify({ emailNormalized: EMAIL, subject: "123456" }),
    ))).toThrow("canonical JSON");
  });

});
