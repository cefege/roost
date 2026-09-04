// Owns registry transactions, schema creation, and in-place schema upgrades.
// Registry storage initializes this schema before any domain store is exposed.
// Migration SQL preserves the existing tables, constraints, and user version.
import { Database } from "bun:sqlite";
import {
  ROUTE_KEY_RE,
  SaasRegistryError,
  assertTenantRouteKey,
} from "./registry-validation.ts";

export function immediate<T>(sqlite: Database, action: () => T): T {
  sqlite.exec("BEGIN IMMEDIATE");
  try {
    const value = action();
    sqlite.exec("COMMIT");
    return value;
  } catch (error) {
    try { sqlite.exec("ROLLBACK"); } catch { /* preserve original failure */ }
    throw error;
  }
}

export function allocateTenantRouteKey(
  sqlite: Database,
  createRouteKey: () => string,
  reserved?: ReadonlySet<string>,
): string {
  for (let attempt = 0; attempt < 128; attempt++) {
    const candidate = assertTenantRouteKey(createRouteKey());
    if (reserved?.has(candidate)) continue;
    const existing = sqlite.query("SELECT 1 FROM accounts WHERE route_key = ?").get(candidate);
    if (!existing) return candidate;
  }
  throw new SaasRegistryError("could not allocate a unique tenant route key", "conflict");
}

export function initialize(sqlite: Database, createRouteKey: () => string): void {
  sqlite.exec("PRAGMA foreign_keys=ON");
  sqlite.exec("PRAGMA busy_timeout=5000");
  sqlite.exec("PRAGMA journal_mode=WAL");
  immediate(sqlite, () => {
    const version = sqlite.query("PRAGMA user_version").get() as { user_version: number };
    if (!Number.isSafeInteger(version.user_version) || version.user_version < 0 || version.user_version > 3) {
      throw new SaasRegistryError("registry schema version is unsupported", "corrupt");
    }
    sqlite.exec(`
      CREATE TABLE IF NOT EXISTS accounts (
        id TEXT PRIMARY KEY,
        email_normalized TEXT NOT NULL UNIQUE,
        route_key TEXT NOT NULL UNIQUE
          CHECK(typeof(route_key) = 'text' AND length(route_key) = 64 AND route_key NOT GLOB '*[^0-9a-f]*'),
        state TEXT NOT NULL CHECK(state IN ('pending','active','disabled')),
        created_at_ms INTEGER NOT NULL,
        activated_at_ms INTEGER,
        disabled_at_ms INTEGER
      );
      CREATE TABLE IF NOT EXISTS coordinators (
        id TEXT PRIMARY KEY,
        account_id TEXT NOT NULL REFERENCES accounts(id) ON DELETE RESTRICT,
        ordinal INTEGER NOT NULL CHECK(ordinal >= 1),
        hostname TEXT NOT NULL UNIQUE,
        container_name TEXT NOT NULL UNIQUE,
        data_dir TEXT NOT NULL,
        image_digest TEXT NOT NULL,
        state TEXT NOT NULL CHECK(state IN ('reserved','seeded','running','routed','invited','active','disabled','failed')),
        created_at_ms INTEGER NOT NULL,
        seeded_at_ms INTEGER,
        running_at_ms INTEGER,
        routed_at_ms INTEGER,
        invited_at_ms INTEGER,
        activated_at_ms INTEGER,
        disabled_at_ms INTEGER,
        failed_at_ms INTEGER,
        updated_at_ms INTEGER NOT NULL,
        last_error TEXT,
        UNIQUE(account_id, ordinal)
      );
      CREATE TABLE IF NOT EXISTS operation_leases (
        coordinator_id TEXT PRIMARY KEY REFERENCES coordinators(id) ON DELETE CASCADE,
        operation TEXT NOT NULL,
        owner TEXT NOT NULL,
        acquired_at_ms INTEGER NOT NULL,
        expires_at_ms INTEGER NOT NULL,
        CHECK(expires_at_ms > acquired_at_ms)
      );
      CREATE TABLE IF NOT EXISTS global_leases (
        resource TEXT PRIMARY KEY,
        operation TEXT NOT NULL,
        owner TEXT NOT NULL,
        acquired_at_ms INTEGER NOT NULL,
        expires_at_ms INTEGER NOT NULL,
        CHECK(expires_at_ms > acquired_at_ms)
      );
    `);

    const accountColumns = sqlite.query("PRAGMA table_info(accounts)").all() as Array<{ name: string }>;
    if (!accountColumns.some((column) => column.name === "route_key")) {
      sqlite.exec("ALTER TABLE accounts ADD COLUMN route_key TEXT");
    }

    const rows = sqlite.query("SELECT id, route_key FROM accounts ORDER BY id").all() as Array<{
      id: string;
      route_key: unknown;
    }>;
    const routeKeys = new Set<string>();
    for (const row of rows) {
      if (row.route_key === null) continue;
      if (typeof row.route_key !== "string" || !ROUTE_KEY_RE.test(row.route_key)) {
        throw new SaasRegistryError("registry row has invalid tenant route key", "corrupt");
      }
      if (routeKeys.has(row.route_key)) {
        throw new SaasRegistryError("registry rows have duplicate tenant route keys", "corrupt");
      }
      routeKeys.add(row.route_key);
    }
    for (const row of rows) {
      if (row.route_key !== null) continue;
      const routeKey = allocateTenantRouteKey(sqlite, createRouteKey, routeKeys);
      sqlite.query("UPDATE accounts SET route_key = ? WHERE id = ? AND route_key IS NULL").run(routeKey, row.id);
      routeKeys.add(routeKey);
    }

    sqlite.exec(`
      CREATE UNIQUE INDEX IF NOT EXISTS accounts_route_key_unique ON accounts(route_key);
      CREATE TRIGGER IF NOT EXISTS accounts_route_key_insert_valid
      BEFORE INSERT ON accounts
      FOR EACH ROW
      WHEN NEW.route_key IS NULL
        OR typeof(NEW.route_key) != 'text'
        OR length(NEW.route_key) != 64
        OR NEW.route_key GLOB '*[^0-9a-f]*'
      BEGIN
        SELECT RAISE(ABORT, 'invalid account route key');
      END;
      CREATE TRIGGER IF NOT EXISTS accounts_route_key_update_immutable
      BEFORE UPDATE OF route_key ON accounts
      FOR EACH ROW
      WHEN NEW.route_key IS NOT OLD.route_key
      BEGIN
        SELECT RAISE(ABORT, 'account route key is immutable');
      END;
      CREATE TABLE IF NOT EXISTS federated_identities (
        issuer TEXT NOT NULL
          CHECK(issuer = 'https://accounts.google.com'),
        subject TEXT NOT NULL
          CHECK(typeof(subject) = 'text' AND length(subject) BETWEEN 1 AND 255),
        account_id TEXT NOT NULL REFERENCES accounts(id) ON DELETE RESTRICT,
        email_normalized TEXT NOT NULL,
        state TEXT NOT NULL CHECK(state IN ('reserved','active','revoked')),
        created_at_ms INTEGER NOT NULL CHECK(created_at_ms >= 0),
        updated_at_ms INTEGER NOT NULL CHECK(updated_at_ms >= created_at_ms),
        verified_at_ms INTEGER NOT NULL CHECK(verified_at_ms >= 0 AND verified_at_ms <= updated_at_ms),
        PRIMARY KEY (issuer, subject)
      );
      CREATE UNIQUE INDEX IF NOT EXISTS federated_identities_one_live_google_per_account
        ON federated_identities(account_id)
        WHERE issuer = 'https://accounts.google.com' AND state IN ('reserved','active');
      CREATE TRIGGER IF NOT EXISTS federated_identity_owner_immutable
      BEFORE UPDATE OF issuer, subject, account_id ON federated_identities
      FOR EACH ROW
      WHEN NEW.issuer IS NOT OLD.issuer
        OR NEW.subject IS NOT OLD.subject
        OR NEW.account_id IS NOT OLD.account_id
      BEGIN
        SELECT RAISE(ABORT, 'federated identity owner is immutable');
      END;
      CREATE TRIGGER IF NOT EXISTS federated_identity_tombstone_permanent
      BEFORE UPDATE OF state ON federated_identities
      FOR EACH ROW
      WHEN OLD.state = 'revoked' AND NEW.state != 'revoked'
      BEGIN
        SELECT RAISE(ABORT, 'federated identity tombstone is permanent');
      END;
      CREATE TRIGGER IF NOT EXISTS federated_identity_delete_forbidden
      BEFORE DELETE ON federated_identities
      FOR EACH ROW
      BEGIN
        SELECT RAISE(ABORT, 'federated identity tombstone cannot be deleted');
      END;

      CREATE TABLE IF NOT EXISTS provisioning_jobs (
        id TEXT PRIMARY KEY,
        idempotency_key_hash TEXT NOT NULL UNIQUE
          CHECK(typeof(idempotency_key_hash) = 'text'
            AND length(idempotency_key_hash) = 64
            AND idempotency_key_hash NOT GLOB '*[^0-9a-f]*'),
        kind TEXT NOT NULL
          CHECK(kind IN ('verified-email','google-signup','google-login','google-link')),
        email_normalized TEXT NOT NULL,
        identity_issuer TEXT,
        identity_subject TEXT,
        activation_token_hash TEXT
          CHECK(activation_token_hash IS NULL OR (
            typeof(activation_token_hash) = 'text'
            AND length(activation_token_hash) = 64
            AND activation_token_hash NOT GLOB '*[^0-9a-f]*'
          )),
        verified_at_ms INTEGER NOT NULL CHECK(verified_at_ms >= 0),
        account_id TEXT NOT NULL REFERENCES accounts(id) ON DELETE RESTRICT,
        coordinator_id TEXT NOT NULL REFERENCES coordinators(id) ON DELETE RESTRICT,
        state TEXT NOT NULL CHECK(state IN ('pending','running','succeeded','failed')),
        attempts INTEGER NOT NULL DEFAULT 0 CHECK(attempts >= 0),
        next_attempt_at_ms INTEGER NOT NULL CHECK(next_attempt_at_ms >= 0),
        locked_until_ms INTEGER CHECK(locked_until_ms IS NULL OR locked_until_ms >= 0),
        lease_token TEXT,
        last_error TEXT CHECK(last_error IS NULL OR length(CAST(last_error AS BLOB)) <= 2048),
        assertion_purpose TEXT CHECK(assertion_purpose IS NULL OR assertion_purpose IN ('continue','link')),
        assertion_route_key TEXT CHECK(assertion_route_key IS NULL OR (
          typeof(assertion_route_key) = 'text'
          AND length(assertion_route_key) = 64
          AND assertion_route_key NOT GLOB '*[^0-9a-f]*'
        )),
        assertion_device_fp TEXT CHECK(assertion_device_fp IS NULL OR (
          typeof(assertion_device_fp) = 'text'
          AND length(assertion_device_fp) = 64
          AND assertion_device_fp NOT GLOB '*[^0-9a-f]*'
        )),
        assertion_jti TEXT,
        assertion_issued_at_ms INTEGER CHECK(assertion_issued_at_ms IS NULL OR assertion_issued_at_ms >= 0),
        assertion_expires_at_ms INTEGER CHECK(assertion_expires_at_ms IS NULL OR assertion_expires_at_ms >= 0),
        created_at_ms INTEGER NOT NULL CHECK(created_at_ms >= 0),
        updated_at_ms INTEGER NOT NULL CHECK(updated_at_ms >= created_at_ms),
        succeeded_at_ms INTEGER,
        failed_at_ms INTEGER,
        FOREIGN KEY (identity_issuer, identity_subject)
          REFERENCES federated_identities(issuer, subject) ON DELETE RESTRICT,
        CHECK(
          (kind = 'verified-email'
            AND identity_issuer IS NULL AND identity_subject IS NULL AND activation_token_hash IS NOT NULL)
          OR
          (kind IN ('google-signup','google-login','google-link')
            AND identity_issuer = 'https://accounts.google.com'
            AND identity_subject IS NOT NULL
            AND activation_token_hash IS NULL)
        ),
        CHECK(
          (state = 'running' AND locked_until_ms IS NOT NULL AND lease_token IS NOT NULL)
          OR
          (state != 'running' AND locked_until_ms IS NULL AND lease_token IS NULL)
        ),
        CHECK(
          (assertion_purpose IS NULL
            AND assertion_route_key IS NULL
            AND assertion_device_fp IS NULL
            AND assertion_jti IS NULL
            AND assertion_issued_at_ms IS NULL
            AND assertion_expires_at_ms IS NULL)
          OR
          (assertion_purpose IS NOT NULL
            AND assertion_route_key IS NOT NULL
            AND assertion_device_fp IS NOT NULL
            AND assertion_jti IS NOT NULL
            AND assertion_issued_at_ms IS NOT NULL
            AND assertion_expires_at_ms IS NOT NULL
            AND assertion_expires_at_ms > assertion_issued_at_ms
            AND assertion_expires_at_ms - assertion_issued_at_ms <= 300000)
        ),
        CHECK(
          (state = 'succeeded' AND succeeded_at_ms IS NOT NULL AND failed_at_ms IS NULL)
          OR (state = 'failed' AND failed_at_ms IS NOT NULL AND succeeded_at_ms IS NULL)
          OR (state IN ('pending','running') AND succeeded_at_ms IS NULL AND failed_at_ms IS NULL)
        )
      );
      CREATE INDEX IF NOT EXISTS provisioning_jobs_due
        ON provisioning_jobs(state, next_attempt_at_ms, locked_until_ms, id);

      CREATE TABLE IF NOT EXISTS link_ticket_redemptions (
        ticket_jti TEXT PRIMARY KEY,
        account_id TEXT NOT NULL REFERENCES accounts(id) ON DELETE RESTRICT,
        coordinator_id TEXT NOT NULL REFERENCES coordinators(id) ON DELETE RESTRICT,
        device_fp TEXT NOT NULL
          CHECK(typeof(device_fp) = 'text'
            AND length(device_fp) = 64
            AND device_fp NOT GLOB '*[^0-9a-f]*'),
        identity_issuer TEXT NOT NULL,
        identity_subject TEXT NOT NULL,
        state TEXT NOT NULL CHECK(state IN ('reserved','consumed')),
        expires_at_ms INTEGER NOT NULL CHECK(expires_at_ms >= 0),
        FOREIGN KEY (identity_issuer, identity_subject)
          REFERENCES federated_identities(issuer, subject) ON DELETE RESTRICT
      );
      PRAGMA user_version=3;
    `);
  });
}
