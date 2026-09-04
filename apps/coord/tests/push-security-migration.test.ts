import { Database } from "bun:sqlite";
import { expect, test } from "bun:test";
import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { runMigrations } from "../src/db/migrate.ts";

const migrationsDir = join(import.meta.dir, "../migrations");
const priorMigrations = readdirSync(migrationsDir)
  .filter((name) => name.endsWith(".sql") && name < "0022_owner_activation.sql")
  .sort()
  .map((name) => ({
    name: name.slice(0, -4),
    sql: readFileSync(join(migrationsDir, name), "utf8"),
  }));
const migration = {
  name: "0022_owner_activation",
  sql: readFileSync(join(migrationsDir, "0022_owner_activation.sql"), "utf8"),
};

function expectSqlFailure(sqlite: Database, sql: string): void {
  const statement = sqlite.prepare(sql);
  try {
    expect(() => statement.run()).toThrow();
  } finally {
    statement.finalize();
  }
}

test("0022 installs owner activation while preserving Push cascade and bounded audit cleanup", async () => {
  const sqlite = new Database(":memory:");
  sqlite.exec("PRAGMA foreign_keys = ON");
  try {
    await runMigrations(sqlite, priorMigrations);

    const validFingerprint = "aa".repeat(32);
    const orphanFingerprint = "bb".repeat(32);
    sqlite.query(
      "INSERT INTO authorized_keys (fingerprint, public_key, label, added_at) VALUES (?, ?, ?, ?)",
    ).run(validFingerprint, new Uint8Array(32), "valid device", 1);
    const insertPush = sqlite.query(`
      INSERT INTO push_subscriptions
        (dashboard_id, viewer_fp, endpoint, p256dh, auth, created_at_ms)
      VALUES (NULL, ?, ?, 'p256dh', 'auth', 1)
    `);
    insertPush.run(validFingerprint, "https://push.example/valid");
    insertPush.run(orphanFingerprint, "https://push.example/orphan");
    sqlite.query(`
      INSERT INTO email_outbox
        (id, kind, recipient, encrypted_payload, idempotency_key, state, attempts,
         locked_until_ms, lease_token, next_attempt_ms, provider_message_id,
         sent_at_ms, failed_at_ms, last_error)
      VALUES ('legacy-invite', 'invitation', 'old@example.test', 'encrypted',
              'legacy-invite', 'pending', 0, NULL, NULL, 1, NULL, NULL, NULL, NULL)
    `).run();


    const insertAudit = sqlite.query(`
      INSERT INTO audit_log
        (dashboard_id, ts, caller_fp, method, path, status, trace_id)
      VALUES (NULL, 1, ?, ?, ?, ?, NULL)
    `);
    insertAudit.run(null, "GET", "/app", 200);
    insertAudit.run(null, "HEAD", "/assets/app.js", 304);
    insertAudit.run(null, "GET", "/api/db-export", 200);
    insertAudit.run(null, "GET", "/internal/coord-handoff/status", 200);
    insertAudit.run(null, "GET", "/ws/sync", 200);
    insertAudit.run(null, "GET", "/login", 401);
    insertAudit.run(validFingerprint, "GET", "/app", 200);
    insertAudit.run(null, "POST", "/app", 204);
    insertPush.finalize();
    insertAudit.finalize();

    await runMigrations(sqlite, [...priorMigrations, migration]);

    expect(sqlite.query(
      "SELECT viewer_fp, endpoint FROM push_subscriptions ORDER BY endpoint",
    ).all()).toEqual([{
      viewer_fp: validFingerprint,
      endpoint: "https://push.example/valid",
    }]);
    const foreignKeys = sqlite.query("PRAGMA foreign_key_list(push_subscriptions)").all() as Array<{
      table: string;
      from: string;
      on_delete: string;
    }>;
    expect(foreignKeys).toContainEqual(expect.objectContaining({
      table: "authorized_keys",
      from: "viewer_fp",
      on_delete: "CASCADE",
    }));
    expect(sqlite.query(
      "SELECT name FROM sqlite_master WHERE type = 'index' AND name = ?",
    ).get("push_subscriptions_dashboard_viewer_idx")).toEqual({
      name: "push_subscriptions_dashboard_viewer_idx",
    });
    const tables = new Set((sqlite.query(
      "SELECT name FROM sqlite_schema WHERE type = 'table'",
    ).all() as Array<{ name: string }>).map((row) => row.name));
    expect(tables.has("invitation_dashboard_grants")).toBe(false);
    expect(tables.has("invitations")).toBe(false);
    expect(tables.has("owner_activation_tokens")).toBe(true);
    expect(sqlite.query(
      "SELECT COUNT(*) AS count FROM email_outbox WHERE kind = 'invitation'",
    ).get()).toEqual({ count: 0 });

    const ownerActivationColumns = (sqlite.query(
      "PRAGMA table_info(owner_activation_tokens)",
    ).all() as Array<{ name: string }>).map((column) => column.name);
    expect(ownerActivationColumns).toEqual([
      "coordinator_id",
      "account_id",
      "email_normalized",
      "token_hash",
      "outbox_id",
      "created_at_ms",
      "expires_at_ms",
      "accepted_at_ms",
      "revoked_at_ms",
    ]);
    expect(sqlite.query(
      "PRAGMA foreign_key_list(owner_activation_tokens)",
    ).all()).toContainEqual(expect.objectContaining({
      table: "email_outbox",
      from: "outbox_id",
      on_delete: "NO ACTION",
    }));

    sqlite.query(`
      INSERT INTO email_outbox
        (id, kind, recipient, encrypted_payload, idempotency_key, state, attempts,
         locked_until_ms, lease_token, next_attempt_ms, provider_message_id,
         sent_at_ms, failed_at_ms, last_error)
      VALUES ('owner-outbox', 'owner_activation', 'owner@example.test', 'encrypted',
              'owner-outbox', 'pending', 0, NULL, NULL, 1, NULL, NULL, NULL, NULL)
    `).run();
    sqlite.query(`
      INSERT INTO owner_activation_tokens
        (coordinator_id, account_id, email_normalized, token_hash, outbox_id,
         created_at_ms, expires_at_ms, accepted_at_ms, revoked_at_ms)
      VALUES ('coordinator-a', 'account-a', 'owner@example.test', 'hash-a',
              'owner-outbox', 1, 2, NULL, NULL)
    `).run();
    expectSqlFailure(sqlite, `
      INSERT INTO owner_activation_tokens VALUES
        ('coordinator-b', 'account-a', 'other@example.test', 'hash-b',
         'owner-outbox', 1, 2, NULL, NULL)
    `);
    expectSqlFailure(sqlite, `
      INSERT INTO owner_activation_tokens VALUES
        ('coordinator-b', 'account-b', 'owner@example.test', 'hash-b',
         'owner-outbox', 1, 2, NULL, NULL)
    `);
    expectSqlFailure(sqlite, `
      INSERT INTO owner_activation_tokens VALUES
        ('coordinator-b', 'account-b', 'other@example.test', 'hash-a',
         'owner-outbox', 1, 2, NULL, NULL)
    `);
    expectSqlFailure(sqlite, `
      INSERT INTO owner_activation_tokens VALUES
        ('coordinator-b', 'account-b', 'other@example.test', 'hash-b',
         'missing-outbox', 1, 2, NULL, NULL)
    `);
    expectSqlFailure(sqlite, "DELETE FROM email_outbox WHERE id = 'owner-outbox'");


    expect(sqlite.query(
      "SELECT caller_fp, method, path, status FROM audit_log ORDER BY id",
    ).all()).toEqual([
      { caller_fp: null, method: "GET", path: "/api/db-export", status: 200 },
      { caller_fp: null, method: "GET", path: "/internal/coord-handoff/status", status: 200 },
      { caller_fp: null, method: "GET", path: "/ws/sync", status: 200 },
      { caller_fp: null, method: "GET", path: "/login", status: 401 },
      { caller_fp: validFingerprint, method: "GET", path: "/app", status: 200 },
      { caller_fp: null, method: "POST", path: "/app", status: 204 },
    ]);

    sqlite.query("DELETE FROM authorized_keys WHERE fingerprint = ?").run(validFingerprint);
    expect(sqlite.query("SELECT COUNT(*) AS count FROM push_subscriptions").get())
      .toEqual({ count: 0 });
  } finally {
    sqlite.close(true);
  }
});
