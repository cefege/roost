// Covers profile gating and pending-activation states for managed-container startup.
// Bun discovers this suite directly; shared database seeds live in the prefixed fixture.
// The invariant API and migrated coord schema define all asserted startup behavior.
import { describe, expect, test } from "bun:test";
import { Database } from "bun:sqlite";
import {
  assertManagedContainerInvariant,
  MANAGED_RUNTIME_SCOPE_TABLES,
} from "../src/managed-container-invariant.ts";
import {
  ACCOUNT_ID,
  EMAIL,
  INSTANCE_ID,
  MANAGED_CONFIG,
  NOW,
  OTHER_INSTANCE_ID,
  OWNER_FP,
  databases,
  insertRuntimeRow,
  migratedDatabase,
  seedActivation,
} from "./managed-container-invariant-database-fixture.ts";

describe("managed-container startup invariant", () => {
  test("is a no-op for generic SaaS and self-hosted profiles", () => {
    const sqlite = new Database(":memory:");
    databases.push(sqlite);
    expect(() => assertManagedContainerInvariant(sqlite, {
      managedContainer: false,
      instanceId: undefined,
    })).not.toThrow();
  });

  test("rejects a managed startup without an exact tenant route key before database access", () => {
    const sqlite = new Database(":memory:");
    databases.push(sqlite);
    for (const tenantRouteKey of [
      undefined,
      "",
      "c".repeat(63),
      "c".repeat(65),
      "C".repeat(64),
      `${"c".repeat(63)}z`,
    ]) {
      expect(
        () => assertManagedContainerInvariant(sqlite, {
          managedContainer: true,
          instanceId: INSTANCE_ID,
          tenantRouteKey,
        }, NOW),
        String(tenantRouteKey),
      ).toThrow("managed profile has no valid tenant route key");
    }
  });

  test("accepts one live pending activation and remains valid on restart", async () => {
    const sqlite = await migratedDatabase();
    seedActivation(sqlite);
    sqlite.query(`
      INSERT INTO audit_log (
        dashboard_id, ts, caller_fp, method, path, status, trace_id
      ) VALUES (NULL, ?, NULL, 'POST', '/public/pre-activation', 403, NULL)
    `).run(NOW);
    sqlite.query(`
      INSERT INTO app_settings (dashboard_id, key, value, updated_at_ms)
      VALUES (NULL, 'push.vapid', '{}', ?)
    `).run(NOW);
    expect(() => assertManagedContainerInvariant(sqlite, MANAGED_CONFIG, NOW)).not.toThrow();
    expect(() => assertManagedContainerInvariant(sqlite, MANAGED_CONFIG, NOW)).not.toThrow();
  });

  for (const [name, mutate] of [
    ["missing", (sqlite: Database) => sqlite.exec("DELETE FROM owner_activation_tokens")],
    ["expired", (sqlite: Database) => sqlite.query(
      "UPDATE owner_activation_tokens SET expires_at_ms = ?",
    ).run(NOW)],
    ["revoked", (sqlite: Database) => sqlite.query(
      "UPDATE owner_activation_tokens SET revoked_at_ms = ?",
    ).run(NOW)],
    ["accepted", (sqlite: Database) => sqlite.query(
      "UPDATE owner_activation_tokens SET accepted_at_ms = ?",
    ).run(NOW)],
    ["foreign", (sqlite: Database) => sqlite.query(
      "UPDATE owner_activation_tokens SET coordinator_id = ?",
    ).run(OTHER_INSTANCE_ID)],
  ] as const) {
    test(`rejects a ${name} pending activation`, async () => {
      const sqlite = await migratedDatabase();
      seedActivation(sqlite);
      mutate(sqlite);
      expect(() => assertManagedContainerInvariant(sqlite, MANAGED_CONFIG, NOW))
        .toThrow("managed-container invariant violation");
    });
  }

  test("requires the pending activation to match exactly one activation outbox", async () => {
    for (const mutation of [
      "UPDATE email_outbox SET kind = 'password_reset'",
      "UPDATE email_outbox SET recipient = 'other@example.test'",
      `INSERT INTO email_outbox (
        id, kind, recipient, encrypted_payload, idempotency_key, state,
        attempts, next_attempt_ms
       ) VALUES (
        'extra-outbox', 'owner_activation', '${EMAIL}', 'ciphertext',
        'extra-outbox', 'pending', 0, ${Number.MAX_SAFE_INTEGER}
       )`,
    ]) {
      const sqlite = await migratedDatabase();
      seedActivation(sqlite);
      sqlite.exec(mutation);
      expect(() => assertManagedContainerInvariant(sqlite, MANAGED_CONFIG, NOW))
        .toThrow("owner activation outbox");
    }
  });

  test("rejects tenant topology and every runtime row while pending", async () => {
    const withOrganization = await migratedDatabase();
    seedActivation(withOrganization);
    withOrganization.query(`
      INSERT INTO organizations (id, slug, name, status, created_at_ms)
      VALUES (?, 'personal', ?, 'active', ?)
    `).run(ACCOUNT_ID, EMAIL, NOW);
    expect(() => assertManagedContainerInvariant(withOrganization, MANAGED_CONFIG, NOW))
      .toThrow("pending coordinator contains organizations rows");

    const withKey = await migratedDatabase();
    seedActivation(withKey);
    withKey.query(`
      INSERT INTO authorized_keys (fingerprint, public_key, label, added_at)
      VALUES (?, ?, 'orphan', ?)
    `).run(OWNER_FP, Buffer.alloc(32), NOW);
    expect(() => assertManagedContainerInvariant(withKey, MANAGED_CONFIG, NOW))
      .toThrow("pending coordinator contains authorized_keys rows");

    for (const table of MANAGED_RUNTIME_SCOPE_TABLES) {
      const sqlite = await migratedDatabase();
      seedActivation(sqlite);
      insertRuntimeRow(sqlite, table, INSTANCE_ID);
      expect(
        () => assertManagedContainerInvariant(sqlite, MANAGED_CONFIG, NOW),
        table,
      ).toThrow("pending coordinator contains");
    }
  });
});
