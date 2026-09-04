// This suite protects the persistent SaaS registry schema, account reservation, and lease invariants.
// Keeping storage and coordinator cases together makes migrations and compare-and-set behavior easy to audit.
// Federated identity and provisioning workflows live in the companion suite to keep this file under the cap.

import { afterEach, describe, expect, test } from "bun:test";
import { Database } from "bun:sqlite";
import { statSync } from "node:fs";
import {
  SaasRegistry,
  SaasRegistryError,
  assertImmutableImageDigest,
  assertTenantRouteKey,
  coordinatorContainerName,
  coordinatorDataDir,
  coordinatorHostname,
} from "../src/saas/registry.ts";
import {
  ACCOUNT_ID,
  COORDINATOR_ID,
  IMAGE,
  ROUTE_A,
  ROUTE_B,
  SECOND_ACCOUNT_ID,
  SECOND_COORDINATOR_ID,
  createSaasRegistryFixtureScope,
} from "./saas-registry-fixtures.ts";

const { cleanup, fixture, rawAccountsFixture } = createSaasRegistryFixtureScope();
afterEach(cleanup);

describe("SaaS registry", () => {
  test("creates a mode-locked schema without secret or token columns", () => {
    const opened = fixture();
    try {
      expect(statSync(opened.root).mode & 0o777).toBe(0o700);
      expect(statSync(opened.path).mode & 0o777).toBe(0o600);
      const sqlite = new Database(opened.path, { readonly: true });
      try {
        const tables = (sqlite.query(
          "SELECT name FROM sqlite_schema WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
        ).all() as Array<{ name: string }>).map((row) => row.name);
        expect(tables).toEqual([
          "accounts",
          "coordinators",
          "federated_identities",
          "global_leases",
          "link_ticket_redemptions",
          "operation_leases",
          "provisioning_jobs",
        ]);
        const coordinatorColumns = (sqlite.query("PRAGMA table_info(coordinators)").all() as Array<{ name: string }>)
          .map((row) => row.name);
        expect(coordinatorColumns).toContain("image_digest");
        expect(coordinatorColumns.some((name) => /token|secret|password|key/i.test(name))).toBe(false);
        const accountColumns = (sqlite.query("PRAGMA table_info(accounts)").all() as Array<{ name: string }>)
          .map((row) => row.name);
        expect(accountColumns).toContain("route_key");
        const versionRow = sqlite.query<{ user_version: number }, []>("PRAGMA user_version").get();
        expect(versionRow?.user_version).toBe(3);
      } finally {
        sqlite.close(true);
      }
    } finally {
      opened.registry.close();
    }
  });

  test("reserves one exact ordinal-one coordinator and resumes it", () => {
    const opened = fixture();
    try {
      const first = opened.registry.reserveAccount(" Owner@Example.COM ", IMAGE);
      expect(first.resumed).toBe(false);
      expect(first.account).toEqual(expect.objectContaining({
        id: ACCOUNT_ID,
        emailNormalized: "owner@example.com",
        state: "pending",
        routeKey: expect.stringMatching(/^[0-9a-f]{64}$/),
      }));
      expect(first.coordinator).toEqual(expect.objectContaining({
        id: COORDINATOR_ID,
        accountId: ACCOUNT_ID,
        ordinal: 1,
        routeKey: first.account.routeKey,
        hostname: coordinatorHostname(COORDINATOR_ID),
        containerName: coordinatorContainerName(COORDINATOR_ID),
        dataDir: coordinatorDataDir(opened.root, COORDINATOR_ID),
        imageDigest: IMAGE,
        state: "reserved",
      }));
      const resumed = opened.registry.reserveAccount("owner@example.com", `sha256:${"b".repeat(64)}`);
      expect(resumed.resumed).toBe(true);
      expect(resumed.coordinator.id).toBe(first.coordinator.id);
      expect(resumed.coordinator.imageDigest).toBe(IMAGE);
      expect(resumed.account.routeKey).toBe(first.account.routeKey);
      expect(resumed.coordinator.routeKey).toBe(first.account.routeKey);
      expect(opened.registry.getRouteKeyByEmail(" OWNER@EXAMPLE.COM ")).toBe(first.account.routeKey);
      expect(opened.registry.getRouteKeyByEmail("unknown@example.com")).toBeNull();
      expect(opened.registry.listAccounts()).toHaveLength(1);
      expect(opened.registry.listCoordinators()).toHaveLength(1);
    } finally {
      opened.registry.close();
    }
  });

  test("backfills one random route key once and preserves it across restarts", () => {
    const raw = rawAccountsFixture([
      { id: ACCOUNT_ID, email: "owner@example.com" },
    ], false);
    let generated = 0;
    const migrated = new SaasRegistry({
      rootDir: raw.root,
      path: raw.path,
      createRouteKey: () => {
        generated++;
        return ROUTE_A;
      },
    });
    expect(migrated.getAccount(ACCOUNT_ID).routeKey).toBe(ROUTE_A);
    expect(generated).toBe(1);
    migrated.close();

    const restarted = new SaasRegistry({
      rootDir: raw.root,
      path: raw.path,
      createRouteKey: () => {
        throw new Error("a persisted route key must not be regenerated");
      },
    });
    try {
      expect(restarted.getAccount(ACCOUNT_ID).routeKey).toBe(ROUTE_A);
      expect(restarted.getRouteKeyByEmail("OWNER@example.com")).toBe(ROUTE_A);
    } finally {
      restarted.close();
    }
  });

  test("rejects invalid or duplicate persisted keys and prevents mutation", () => {
    for (const invalid of ["A".repeat(64), "a".repeat(63), `${"a".repeat(63)}g`, ""]) {
      expect(() => assertTenantRouteKey(invalid)).toThrow(SaasRegistryError);
      const raw = rawAccountsFixture([
        { id: ACCOUNT_ID, email: "owner@example.com", routeKey: invalid },
      ], true);
      expect(() => new SaasRegistry({ rootDir: raw.root, path: raw.path }))
        .toThrow("invalid tenant route key");
    }

    const duplicate = rawAccountsFixture([
      { id: ACCOUNT_ID, email: "owner@example.com", routeKey: ROUTE_A },
      { id: SECOND_ACCOUNT_ID, email: "other@example.com", routeKey: ROUTE_A },
    ], true);
    expect(() => new SaasRegistry({ rootDir: duplicate.root, path: duplicate.path }))
      .toThrow("duplicate tenant route keys");

    const opened = fixture();
    try {
      const reserved = opened.registry.reserveAccount("owner@example.com", IMAGE);
      const sqlite = new Database(opened.path);
      try {
        expect(() => sqlite.query("UPDATE accounts SET route_key = ? WHERE id = ?")
          .run(ROUTE_B, ACCOUNT_ID)).toThrow("immutable");
      } finally {
        sqlite.close();
      }
      expect(opened.registry.getAccount(ACCOUNT_ID).routeKey).toBe(reserved.account.routeKey);
    } finally {
      opened.registry.close();
    }
  });

  test("concurrent handles converge and active or disabled emails fail precisely", () => {
    const opened = fixture();
    const second = new SaasRegistry({ rootDir: opened.root, path: opened.path, now: () => opened.nowRef.value });
    try {
      const reserved = opened.registry.reserveAccount("owner@example.com", IMAGE);
      expect(second.reserveAccount("OWNER@example.com", IMAGE).coordinator.id).toBe(reserved.coordinator.id);
      opened.registry.markAccountActive(ACCOUNT_ID);
      expect(() => second.reserveAccount("owner@example.com", IMAGE)).toThrow("already active");
      opened.registry.disableAccount(ACCOUNT_ID);
      expect(() => second.reserveAccount("owner@example.com", IMAGE)).toThrow("disabled");
    } finally {
      second.close();
      opened.registry.close();
    }
  });

  test("schema permits a future second ordinal but reserve never invents it", () => {
    const opened = fixture();
    try {
      opened.registry.reserveAccount("owner@example.com", IMAGE);
      const sqlite = new Database(opened.path);
      try {
        sqlite.exec("PRAGMA foreign_keys=ON");
        sqlite.query(`
          INSERT INTO coordinators (
            id, account_id, ordinal, hostname, container_name, data_dir, image_digest,
            state, created_at_ms, updated_at_ms
          ) VALUES (?, ?, 2, ?, ?, ?, ?, 'reserved', 1000, 1000)
        `).run(
          SECOND_COORDINATOR_ID,
          ACCOUNT_ID,
          coordinatorHostname(SECOND_COORDINATOR_ID),
          coordinatorContainerName(SECOND_COORDINATOR_ID),
          coordinatorDataDir(opened.root, SECOND_COORDINATOR_ID),
          IMAGE,
        );
      } finally {
        sqlite.close(true);
      }
      expect(opened.registry.reserveAccount("owner@example.com", IMAGE).coordinator.ordinal).toBe(1);
      expect(opened.registry.listCoordinators()).toHaveLength(2);
    } finally {
      opened.registry.close();
    }
  });

  test("enforces immutable image IDs and compare-and-set transitions", () => {
    for (const invalid of [
      "roost-coord:latest",
      "registry.example/roost@sha256:" + "a".repeat(64),
      "sha256:" + "A".repeat(64),
      "sha256:" + "a".repeat(63),
      "sha512:" + "a".repeat(64),
    ]) {
      expect(() => assertImmutableImageDigest(invalid)).toThrow(SaasRegistryError);
    }
    expect(assertImmutableImageDigest(IMAGE)).toBe(IMAGE);

    const opened = fixture();
    try {
      opened.registry.reserveAccount("owner@example.com", IMAGE);
      opened.nowRef.value = 2_000;
      expect(opened.registry.transitionCoordinator(COORDINATOR_ID, "reserved", "seeded")).toEqual(
        expect.objectContaining({ state: "seeded", seededAtMs: 2_000, updatedAtMs: 2_000 }),
      );
      expect(() => opened.registry.transitionCoordinator(COORDINATOR_ID, "reserved", "running"))
        .toThrow("state changed concurrently");
      expect(opened.registry.setCoordinatorError(COORDINATOR_ID, "seed failed").lastError).toBe("seed failed");
      expect(() => opened.registry.setCoordinatorError(COORDINATOR_ID, "bad\nsecret"))
        .toThrow("invalid redacted coordinator error");
    } finally {
      opened.registry.close();
    }
  });

  test("serializes leases and permits only expired stale takeover", () => {
    const opened = fixture();
    try {
      opened.registry.reserveAccount("owner@example.com", IMAGE);
      expect(opened.registry.acquireLease(COORDINATOR_ID, "reconcile", "operator-a", 500)).toEqual(
        expect.objectContaining({ owner: "operator-a", acquiredAtMs: 1_000, expiresAtMs: 1_500 }),
      );
      expect(() => opened.registry.acquireLease(COORDINATOR_ID, "rollout", "operator-b", 500))
        .toThrow("lease is held");
      opened.nowRef.value = 1_200;
      expect(opened.registry.renewLease(COORDINATOR_ID, "reconcile", "operator-a", 600).expiresAtMs).toBe(1_800);
      opened.nowRef.value = 1_800;
      expect(opened.registry.acquireLease(COORDINATOR_ID, "rollout", "operator-b", 400).owner).toBe("operator-b");
      expect(() => opened.registry.releaseLease(COORDINATOR_ID, "reconcile", "operator-a"))
        .toThrow("lease was lost");
      opened.registry.releaseLease(COORDINATOR_ID, "rollout", "operator-b");
      expect(opened.registry.getLease(COORDINATOR_ID)).toBeNull();
      expect(opened.registry.acquireGlobalLease("tenant-routes", "route-update", "route-a", 500).owner)
        .toBe("route-a");
      expect(() => opened.registry.acquireGlobalLease("tenant-routes", "route-update", "route-b", 500))
        .toThrow("global operation lease is held");
      opened.nowRef.value = 2_300;
      expect(opened.registry.acquireGlobalLease("tenant-routes", "route-update", "route-b", 500).owner)
        .toBe("route-b");
      opened.registry.releaseGlobalLease("tenant-routes", "route-update", "route-b");
    } finally {
      opened.registry.close();
    }
  });

  test("rejects corrupted UUID-derived fields on every read", () => {
    const opened = fixture();
    try {
      opened.registry.reserveAccount("owner@example.com", IMAGE);
      const sqlite = new Database(opened.path);
      try {
        sqlite.query("UPDATE coordinators SET hostname = 'attacker.example' WHERE id = ?").run(COORDINATOR_ID);
      } finally {
        sqlite.close(true);
      }
      expect(() => opened.registry.getCoordinator(COORDINATOR_ID)).toThrow("mismatched coordinator hostname");
      expect(() => opened.registry.listCoordinators()).toThrow("mismatched coordinator hostname");
    } finally {
      opened.registry.close();
    }
  });

  test("upgrades a populated v2 database to the v3 identity and queue schema", () => {
    const opened = fixture();
    opened.registry.reserveAccount("owner@example.com", IMAGE);
    opened.registry.close();
    const v2 = new Database(opened.path);
    try {
      v2.exec(`
        DROP TABLE link_ticket_redemptions;
        DROP TABLE provisioning_jobs;
        DROP TABLE federated_identities;
        PRAGMA user_version=2;
      `);
    } finally {
      v2.close(true);
    }

    const migrated = new SaasRegistry({
      rootDir: opened.root,
      path: opened.path,
      createRouteKey: () => {
        throw new Error("v2 route keys must not be regenerated");
      },
    });
    try {
      expect(migrated.getAccount(ACCOUNT_ID).routeKey).toMatch(/^[0-9a-f]{64}$/);
      const sqlite = new Database(opened.path, { readonly: true });
      try {
        const versionRow = sqlite.query<{ user_version: number }, []>("PRAGMA user_version").get();
        expect(versionRow?.user_version).toBe(3);
        const tables = sqlite.query(`
          SELECT name FROM sqlite_schema
          WHERE type = 'table' AND name IN (
            'federated_identities',
            'provisioning_jobs',
            'link_ticket_redemptions'
          )
          ORDER BY name
        `).all() as Array<{ name: string }>;
        expect(tables.map((row) => row.name)).toEqual([
          "federated_identities",
          "link_ticket_redemptions",
          "provisioning_jobs",
        ]);
      } finally {
        sqlite.close(true);
      }
    } finally {
      migrated.close();
    }
  });
});
