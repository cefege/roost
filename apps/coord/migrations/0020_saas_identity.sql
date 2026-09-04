-- Creates the account, organization, and dashboard identity authority.
-- Later migrations attach legacy runtime rows to dashboards and harden scopes.
-- The migration runner owns transaction boundaries and foreign-key enforcement.

CREATE TABLE accounts (
  id TEXT PRIMARY KEY,
  email_normalized TEXT UNIQUE NOT NULL,
  password_hash TEXT,
  status TEXT NOT NULL CHECK(status IN ('active','disabled')),
  created_at_ms INTEGER NOT NULL,
  password_changed_at_ms INTEGER
);
CREATE TABLE account_identities (
  account_id TEXT NOT NULL REFERENCES accounts(id),
  issuer TEXT NOT NULL,
  subject TEXT NOT NULL,
  email_verified INTEGER NOT NULL CHECK(email_verified IN (0, 1)),
  PRIMARY KEY (issuer, subject)
);
CREATE INDEX account_identities_account_idx ON account_identities(account_id);
CREATE TABLE account_devices (
  fingerprint TEXT PRIMARY KEY REFERENCES authorized_keys(fingerprint),
  account_id TEXT NOT NULL REFERENCES accounts(id),
  added_at_ms INTEGER NOT NULL,
  last_seen_at_ms INTEGER NOT NULL
);
CREATE INDEX account_devices_account_idx ON account_devices(account_id);

CREATE TABLE organizations (
  id TEXT PRIMARY KEY,
  slug TEXT UNIQUE NOT NULL,
  name TEXT NOT NULL,
  status TEXT NOT NULL CHECK(status IN ('active','suspended','deleting')),
  created_at_ms INTEGER NOT NULL
);
CREATE TABLE organization_memberships (
  organization_id TEXT NOT NULL REFERENCES organizations(id),
  account_id TEXT NOT NULL REFERENCES accounts(id),
  role TEXT NOT NULL CHECK(role IN ('owner','admin','member')),
  created_at_ms INTEGER NOT NULL,
  PRIMARY KEY (organization_id, account_id)
);
CREATE INDEX organization_memberships_account_idx ON organization_memberships(account_id, organization_id);
CREATE TABLE dashboards (
  id TEXT PRIMARY KEY,
  organization_id TEXT NOT NULL REFERENCES organizations(id),
  slug TEXT NOT NULL,
  name TEXT NOT NULL,
  status TEXT NOT NULL CHECK(status IN ('active','suspended','deleting')),
  created_at_ms INTEGER NOT NULL,
  UNIQUE (organization_id, slug)
);
CREATE INDEX dashboards_organization_idx ON dashboards(organization_id, id);
CREATE TABLE dashboard_memberships (
  dashboard_id TEXT NOT NULL REFERENCES dashboards(id),
  account_id TEXT NOT NULL REFERENCES accounts(id),
  role TEXT NOT NULL CHECK(role IN ('admin','member')),
  created_at_ms INTEGER NOT NULL,
  PRIMARY KEY (dashboard_id, account_id)
);
CREATE INDEX dashboard_memberships_account_idx ON dashboard_memberships(account_id, dashboard_id);

CREATE TABLE invitations (
  id TEXT PRIMARY KEY,
  organization_id TEXT NOT NULL REFERENCES organizations(id),
  email_normalized TEXT NOT NULL,
  organization_role TEXT NOT NULL CHECK(organization_role IN ('owner','admin','member')),
  token_hash TEXT UNIQUE NOT NULL,
  expires_at_ms INTEGER NOT NULL,
  accepted_at_ms INTEGER,
  revoked_at_ms INTEGER,
  created_by_account_id TEXT NOT NULL REFERENCES accounts(id)
);
CREATE INDEX invitations_redeem_idx ON invitations(token_hash, expires_at_ms);
CREATE TABLE invitation_dashboard_grants (
  invitation_id TEXT NOT NULL REFERENCES invitations(id),
  dashboard_id TEXT NOT NULL REFERENCES dashboards(id),
  dashboard_role TEXT NOT NULL CHECK(dashboard_role IN ('admin','member')),
  PRIMARY KEY (invitation_id, dashboard_id)
);
CREATE TABLE password_reset_tokens (
  account_id TEXT NOT NULL REFERENCES accounts(id),
  email_normalized TEXT NOT NULL,
  token_hash TEXT PRIMARY KEY,
  expires_at_ms INTEGER NOT NULL,
  used_at_ms INTEGER
);
CREATE INDEX password_reset_tokens_account_idx ON password_reset_tokens(account_id, expires_at_ms);
CREATE TABLE email_outbox (
  id TEXT PRIMARY KEY,
  kind TEXT NOT NULL,
  recipient TEXT NOT NULL,
  encrypted_payload TEXT NOT NULL,
  idempotency_key TEXT UNIQUE NOT NULL,
  state TEXT NOT NULL CHECK(state IN ('pending','sending','sent','failed')),
  attempts INTEGER NOT NULL DEFAULT 0,
  locked_until_ms INTEGER,
  lease_token TEXT,
  next_attempt_ms INTEGER NOT NULL,
  provider_message_id TEXT,
  sent_at_ms INTEGER,
  failed_at_ms INTEGER,
  last_error TEXT
);
CREATE INDEX email_outbox_due_idx ON email_outbox(state, next_attempt_ms, locked_until_ms);
