import { Database } from "bun:sqlite";
import { expect, test } from "bun:test";
import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { runMigrations } from "../src/db/migrate.ts";

const migrationsDir = join(import.meta.dir, "../migrations");
const priorMigrations = readdirSync(migrationsDir)
  .filter((name) => name.endsWith(".sql") && name < "0023_federated_identity.sql")
  .sort()
  .map((name) => ({
    name: name.slice(0, -4),
    sql: readFileSync(join(migrationsDir, name), "utf8"),
  }));
const migration = {
  name: "0023_federated_identity",
  sql: readFileSync(join(migrationsDir, "0023_federated_identity.sql"), "utf8"),
};

function expectSqlFailure(sqlite: Database, sql: string): void {
  const statement = sqlite.prepare(sql);
  try {
    expect(() => statement.run()).toThrow();
  } finally {
    statement.finalize();
  }
}

test("0023 backfills native identity and coordinator-delivered activation", async () => {
  const sqlite = new Database(":memory:");
  sqlite.exec("PRAGMA foreign_keys = ON");
  try {
    await runMigrations(sqlite, priorMigrations);
    sqlite.query(`
      INSERT INTO accounts
        (id, email_normalized, password_hash, status, created_at_ms, password_changed_at_ms)
      VALUES ('account-a', 'owner@example.test', 'hash', 'active', 11, 11)
    `).run();
    sqlite.query(`
      INSERT INTO account_identities (account_id, issuer, subject, email_verified)
      VALUES ('account-a', 'native', 'account-a', 1)
    `).run();
    sqlite.query(`
      INSERT INTO email_outbox
        (id, kind, recipient, encrypted_payload, idempotency_key, state, attempts,
         locked_until_ms, lease_token, next_attempt_ms, provider_message_id,
         sent_at_ms, failed_at_ms, last_error)
      VALUES ('outbox-a', 'owner_activation', 'owner@example.test', 'ciphertext',
              'owner-a', 'pending', 0, NULL, NULL, 11, NULL, NULL, NULL, NULL)
    `).run();
    sqlite.query(`
      INSERT INTO owner_activation_tokens
        (coordinator_id, account_id, email_normalized, token_hash, outbox_id,
         created_at_ms, expires_at_ms, accepted_at_ms, revoked_at_ms)
      VALUES ('coordinator-a', 'account-a', 'owner@example.test', 'token-a',
              'outbox-a', 11, 22, NULL, NULL)
    `).run();

    await runMigrations(sqlite, [...priorMigrations, migration]);

    expect(sqlite.query(`
      SELECT account_id, issuer, subject, email_normalized, linked_at_ms,
             last_authenticated_at_ms, revoked_at_ms
      FROM account_identities
    `).all()).toEqual([{
      account_id: "account-a",
      issuer: "native",
      subject: "account-a",
      email_normalized: "owner@example.test",
      linked_at_ms: 11,
      last_authenticated_at_ms: null,
      revoked_at_ms: null,
    }]);
    expect(sqlite.query(`
      SELECT outbox_id, delivery FROM owner_activation_tokens
    `).get()).toEqual({ outbox_id: "outbox-a", delivery: "coordinator-email" });
    const outboxColumn = (sqlite.query("PRAGMA table_info(owner_activation_tokens)").all() as Array<{
      name: string;
      notnull: number;
    }>).find((column) => column.name === "outbox_id");
    expect(outboxColumn?.notnull).toBe(0);

    sqlite.query(`
      INSERT INTO account_identities
        (account_id, issuer, subject, email_normalized, linked_at_ms,
         last_authenticated_at_ms, revoked_at_ms)
      VALUES ('account-a', 'https://accounts.google.com', 'google-subject-a',
              'owner@example.test', 12, NULL, NULL)
    `).run();
    expectSqlFailure(sqlite, `
      INSERT INTO account_identities
        (account_id, issuer, subject, email_normalized, linked_at_ms,
         last_authenticated_at_ms, revoked_at_ms)
      VALUES ('account-a', 'https://accounts.google.com', 'google-subject-b',
              'owner@example.test', 13, NULL, NULL)
    `);
    sqlite.query(`
      UPDATE account_identities SET revoked_at_ms = 14
      WHERE issuer = 'https://accounts.google.com' AND subject = 'google-subject-a'
    `).run();
    sqlite.query(`
      INSERT INTO account_identities
        (account_id, issuer, subject, email_normalized, linked_at_ms,
         last_authenticated_at_ms, revoked_at_ms)
      VALUES ('account-a', 'https://accounts.google.com', 'google-subject-b',
              'owner@example.test', 15, NULL, NULL)
    `).run();

    sqlite.query(`
      INSERT INTO owner_activation_tokens
        (coordinator_id, account_id, email_normalized, token_hash, outbox_id, delivery,
         created_at_ms, expires_at_ms, accepted_at_ms, revoked_at_ms)
      VALUES ('coordinator-b', 'account-b', 'new@example.test', 'token-b', NULL,
              'signup-gateway', 12, 23, NULL, NULL)
    `).run();
    expectSqlFailure(sqlite, `
      INSERT INTO owner_activation_tokens
        (coordinator_id, account_id, email_normalized, token_hash, outbox_id, delivery,
         created_at_ms, expires_at_ms, accepted_at_ms, revoked_at_ms)
      VALUES ('coordinator-c', 'account-c', 'bad@example.test', 'token-c', NULL,
              'other', 12, 23, NULL, NULL)
    `);
  } finally {
    sqlite.close(true);
  }
});
