// Covers worker credentials and push ownership for managed-container startup.
// Bun discovers this suite directly and calls the shared prefixed database fixture.
// The invariant API and migrated coord schema define its credential relationships.
import { describe, expect, test } from "bun:test";
import { assertManagedContainerInvariant } from "../src/managed-container-invariant.ts";
import {
  INSTANCE_ID,
  MANAGED_CONFIG,
  NOW,
  OWNER_FP,
  WORKER_FP,
  ensureWorker,
  migratedDatabase,
  seedActiveOwner,
  withoutForeignKeys,
} from "./managed-container-invariant-database-fixture.ts";

describe("managed-container startup invariant", () => {
  test("requires exact active and tombstoned worker credential states", async () => {
    const missingActiveKey = await migratedDatabase();
    seedActiveOwner(missingActiveKey);
    missingActiveKey.query(`
      INSERT INTO workers (
        fp, dashboard_id, label, os, registered_at_ms, last_seen_ms
      ) VALUES (?, ?, 'Worker', 'linux', ?, ?)
    `).run(WORKER_FP, INSTANCE_ID, NOW, NOW);
    expect(() => assertManagedContainerInvariant(missingActiveKey, MANAGED_CONFIG, NOW))
      .toThrow("active worker has no matching authorized key");

    const revokedActiveWorker = await migratedDatabase();
    seedActiveOwner(revokedActiveWorker);
    ensureWorker(revokedActiveWorker);
    revokedActiveWorker.query(`
      INSERT INTO authorized_key_revocations (
        fingerprint, revoked_at_ms, revoked_by_fp, reason
      ) VALUES (?, ?, ?, 'test')
    `).run(WORKER_FP, NOW, OWNER_FP);
    expect(() => assertManagedContainerInvariant(revokedActiveWorker, MANAGED_CONFIG, NOW))
      .toThrow("active worker has a matching revocation");

    const validTombstone = await migratedDatabase();
    seedActiveOwner(validTombstone);
    ensureWorker(validTombstone);
    validTombstone.query(`
      INSERT INTO authorized_key_revocations (
        fingerprint, revoked_at_ms, revoked_by_fp, reason
      ) VALUES (?, ?, ?, 'worker-deleted')
    `).run(WORKER_FP, NOW, OWNER_FP);
    validTombstone.query(
      "UPDATE workers SET deleted_at_ms = ? WHERE fp = ?",
    ).run(NOW, WORKER_FP);
    validTombstone.query(
      "DELETE FROM authorized_keys WHERE fingerprint = ?",
    ).run(WORKER_FP);
    expect(
      () => assertManagedContainerInvariant(validTombstone, MANAGED_CONFIG, NOW),
    ).not.toThrow();

    const tombstoneWithoutRevocation = await migratedDatabase();
    seedActiveOwner(tombstoneWithoutRevocation);
    ensureWorker(tombstoneWithoutRevocation);
    tombstoneWithoutRevocation.query(
      "UPDATE workers SET deleted_at_ms = ? WHERE fp = ?",
    ).run(NOW, WORKER_FP);
    tombstoneWithoutRevocation.query(
      "DELETE FROM authorized_keys WHERE fingerprint = ?",
    ).run(WORKER_FP);
    expect(
      () => assertManagedContainerInvariant(
        tombstoneWithoutRevocation,
        MANAGED_CONFIG,
        NOW,
      ),
    ).toThrow("tombstoned worker has no matching revocation");

    const keyedTombstone = await migratedDatabase();
    seedActiveOwner(keyedTombstone);
    ensureWorker(keyedTombstone);
    keyedTombstone.query(`
      INSERT INTO authorized_key_revocations (
        fingerprint, revoked_at_ms, revoked_by_fp, reason
      ) VALUES (?, ?, ?, 'worker-deleted')
    `).run(WORKER_FP, NOW, OWNER_FP);
    keyedTombstone.query(
      "UPDATE workers SET deleted_at_ms = ? WHERE fp = ?",
    ).run(NOW, WORKER_FP);
    expect(() => assertManagedContainerInvariant(keyedTombstone, MANAGED_CONFIG, NOW))
      .toThrow("tombstoned worker retains an authorized key");
  });

  test("classifies authorized keys through account devices or active workers only", async () => {
    const orphanKey = await migratedDatabase();
    seedActiveOwner(orphanKey);
    orphanKey.query(`
      INSERT INTO authorized_keys (fingerprint, public_key, label, added_at)
      VALUES (?, ?, 'Orphan', ?)
    `).run(WORKER_FP, Buffer.alloc(32, 2), NOW);
    expect(() => assertManagedContainerInvariant(orphanKey, MANAGED_CONFIG, NOW))
      .toThrow("authorized key is not owned by exactly one device or active worker");

    const dualKey = await migratedDatabase();
    seedActiveOwner(dualKey);
    withoutForeignKeys(dualKey, () => {
      dualKey.query(`
        INSERT INTO workers (
          fp, dashboard_id, label, os, registered_at_ms, last_seen_ms
        ) VALUES (?, ?, 'Worker', 'linux', ?, ?)
      `).run(OWNER_FP, INSTANCE_ID, NOW, NOW);
    });
    expect(() => assertManagedContainerInvariant(dualKey, MANAGED_CONFIG, NOW))
      .toThrow("authorized key is not owned by exactly one device or active worker");
  });

  test("requires Push subscriptions to belong to the owner account device", async () => {
    const sqlite = await migratedDatabase();
    seedActiveOwner(sqlite);
    withoutForeignKeys(sqlite, () => {
      sqlite.query(`
        INSERT INTO push_subscriptions (
          dashboard_id, viewer_fp, endpoint, p256dh, auth, created_at_ms
        ) VALUES (?, ?, 'https://push.example/orphan', 'p256dh', 'auth', ?)
      `).run(INSTANCE_ID, WORKER_FP, NOW);
    });
    expect(() => assertManagedContainerInvariant(sqlite, MANAGED_CONFIG, NOW))
      .toThrow("push subscription does not belong to an account device");
  });
});
