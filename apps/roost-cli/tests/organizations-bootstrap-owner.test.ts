import { describe, expect, test } from "bun:test";
import { Database } from "bun:sqlite";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { runMigrations } from "../../coord/src/db/migrate.ts";
import {
  bootstrapOwner,
  OWNER_BOOTSTRAP_PASSWORD_ENV,
  parseBootstrapOwnerCommand,
  readOwnerBootstrapPassword,
} from "../src/organizations.ts";

const PASSWORD = "correct horse battery staple";
const EMAIL = "owner@example.test";

async function withMigratedDatabase(
  run: (databasePath: string) => Promise<void> | void,
): Promise<void> {
  const root = await mkdtemp(join(tmpdir(), "roost-bootstrap-owner-"));
  const databasePath = join(root, "coordinator_v2.db");
  const sqlite = new Database(databasePath);
  try {
    await runMigrations(sqlite);
  } finally {
    sqlite.close(true);
  }

  try {
    await run(databasePath);
  } finally {
    await rm(root, { recursive: true, force: true });
  }
}

function sequence(...ids: string[]): () => string {
  let index = 0;
  return () => ids[index++] ?? `unexpected-id-${index}`;
}

function countRows(sqlite: Database, table: string): number {
  const row = sqlite.query(`SELECT COUNT(*) AS count FROM ${table}`).get() as { count: number };
  return row.count;
}

function ownerOptions(databasePath: string) {
  return {
    databasePath,
    input: { email: EMAIL, organization: "roost", dashboard: "personal" },
    password: PASSWORD,
    now: () => 123,
    createId: sequence("account-1", "organization-1", "dashboard-1"),
  };
}

describe("organizations bootstrap-owner", () => {
  test("parses only the exact password-free command shape", () => {
    expect(parseBootstrapOwnerCommand([
      "bootstrap-owner",
      "--email",
      " Owner@Example.Test ",
      "--organization",
      "roost",
      "--dashboard",
      "personal",
    ])).toEqual({ email: EMAIL, organization: "roost", dashboard: "personal" });

    expect(() => parseBootstrapOwnerCommand([
      "bootstrap-owner",
      "--email",
      EMAIL,
      "--organization",
      "roost",
      "--dashboard",
      "personal",
      "--password",
      PASSWORD,
    ])).toThrow("unknown organizations argument: --password");
    expect(() => parseBootstrapOwnerCommand([
      "bootstrap-owner",
      "--email",
      EMAIL,
      "--email",
      "other@example.test",
      "--organization",
      "roost",
      "--dashboard",
      "personal",
    ])).toThrow("duplicate organizations argument: --email");
  });

  test("reads only environment or piped stdin and erases the source", async () => {
    const environment = { [OWNER_BOOTSTRAP_PASSWORD_ENV]: PASSWORD };
    expect(await readOwnerBootstrapPassword({ environment })).toBe(PASSWORD);
    expect(environment[OWNER_BOOTSTRAP_PASSWORD_ENV]).toBeUndefined();

    const bytes = new TextEncoder().encode(`${PASSWORD}\n`);
    expect(await readOwnerBootstrapPassword({ environment: {}, readStdin: async () => bytes })).toBe(PASSWORD);
    expect(bytes.every((byte) => byte === 0)).toBe(true);

    await expect(readOwnerBootstrapPassword({ environment: {}, readStdin: async () => new Uint8Array() }))
      .rejects.toThrow("owner password must be");
  });

  test("atomically creates the native owner on an empty managed database", async () => {
    await withMigratedDatabase(async (databasePath) => {
      const result = await bootstrapOwner(ownerOptions(databasePath));
      expect(result).toMatchObject({
        accountId: "account-1",
        ownerEmailNormalized: EMAIL,
        organizationId: "organization-1",
        dashboardId: "dashboard-1",
        assignments: { workers: 0 },
      });
      const inspect = new Database(databasePath, { readonly: true });
      try {
        const account = inspect.query(
          "SELECT email_normalized, password_hash, status, password_changed_at_ms FROM accounts",
        ).get() as {
          email_normalized: string;
          password_hash: string;
          status: string;
          password_changed_at_ms: number;
        };
        expect(account).toMatchObject({
          email_normalized: EMAIL,
          status: "active",
          password_changed_at_ms: 123,
        });
        expect(account.password_hash).not.toBe(PASSWORD);
        expect(await Bun.password.verify(PASSWORD, account.password_hash)).toBe(true);
        expect(inspect.query(
          `SELECT issuer, subject, email_normalized, linked_at_ms,
                  last_authenticated_at_ms, revoked_at_ms
           FROM account_identities`,
        ).all()).toEqual([{
          issuer: "native",
          subject: "account-1",
          email_normalized: EMAIL,
          linked_at_ms: 123,
          last_authenticated_at_ms: null,
          revoked_at_ms: null,
        }]);
        expect(inspect.query("SELECT slug, status FROM organizations").all())
          .toEqual([{ slug: "roost", status: "active" }]);
        expect(inspect.query("SELECT slug, status FROM dashboards").all())
          .toEqual([{ slug: "personal", status: "active" }]);
        expect(inspect.query("SELECT role FROM organization_memberships").all()).toEqual([{ role: "owner" }]);
        expect(inspect.query("SELECT role FROM dashboard_memberships").all()).toEqual([{ role: "admin" }]);
      } finally {
        inspect.close(true);
      }
    });
  });

  test("rolls back every tenant write when owner creation fails", async () => {
    await withMigratedDatabase(async (databasePath) => {
      const sqlite = new Database(databasePath);
      try {
        sqlite.exec(`CREATE TRIGGER fail_owner_creation
          BEFORE INSERT ON organizations
          BEGIN
            SELECT RAISE(ABORT, 'fixture owner creation failed');
          END`);
      } finally {
        sqlite.close(true);
      }

      await expect(bootstrapOwner(ownerOptions(databasePath))).rejects.toThrow(
        "fixture owner creation failed",
      );
      const inspect = new Database(databasePath, { readonly: true });
      try {
        for (const table of [
          "accounts",
          "account_identities",
          "organizations",
          "organization_memberships",
          "dashboards",
          "dashboard_memberships",
        ]) {
          expect(countRows(inspect, table)).toBe(0);
        }
      } finally {
        inspect.close(true);
      }
    });
  });

  test("refuses both existing accounts and partial tenant state without mutation", async () => {
    for (const existingState of ["account", "organization"] as const) {
      await withMigratedDatabase(async (databasePath) => {
        const sqlite = new Database(databasePath);
        try {
          if (existingState === "account") {
            sqlite.query(`INSERT INTO accounts
              (id, email_normalized, password_hash, status, created_at_ms, password_changed_at_ms)
              VALUES ('existing', 'existing@example.test', NULL, 'active', 1, NULL)`).run();
          } else {
            sqlite.query(
              "INSERT INTO organizations (id, slug, name, status, created_at_ms) VALUES ('existing', 'existing', 'existing', 'active', 1)",
            ).run();
          }
        } finally {
          sqlite.close(true);
        }

        await expect(bootstrapOwner(ownerOptions(databasePath))).rejects.toThrow("refusing bootstrap-owner");
        const inspect = new Database(databasePath, { readonly: true });
        try {
          expect(countRows(inspect, "accounts")).toBe(existingState === "account" ? 1 : 0);
          expect(countRows(inspect, "organizations")).toBe(existingState === "organization" ? 1 : 0);
          expect(countRows(inspect, "dashboards")).toBe(0);
        } finally {
          inspect.close(true);
        }
      });
    }
  });
});
