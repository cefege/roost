-- The legacy identity table only recorded a boolean verified bit. Preserve every
-- assignment while moving the supported native identity onto the durable
-- provider-email/timestamp shape used by federated identities.
CREATE TABLE account_identities_federated (
  account_id TEXT NOT NULL REFERENCES accounts(id),
  issuer TEXT NOT NULL,
  subject TEXT NOT NULL,
  email_normalized TEXT NOT NULL,
  linked_at_ms INTEGER NOT NULL,
  last_authenticated_at_ms INTEGER,
  revoked_at_ms INTEGER,
  PRIMARY KEY (issuer, subject)
);
INSERT INTO account_identities_federated (
  account_id, issuer, subject, email_normalized, linked_at_ms,
  last_authenticated_at_ms, revoked_at_ms
)
SELECT identity.account_id, identity.issuer, identity.subject,
       account.email_normalized, account.created_at_ms, NULL, NULL
FROM account_identities AS identity
JOIN accounts AS account ON account.id = identity.account_id;
DROP TABLE account_identities;
ALTER TABLE account_identities_federated RENAME TO account_identities;

CREATE INDEX account_identities_account_idx
  ON account_identities(account_id);
CREATE UNIQUE INDEX account_identities_active_google_account_unique
  ON account_identities(account_id)
  WHERE issuer = 'https://accounts.google.com' AND revoked_at_ms IS NULL;

CREATE TABLE federated_assertion_redemptions (
  jti TEXT PRIMARY KEY,
  purpose TEXT NOT NULL,
  device_fp TEXT NOT NULL,
  redeemed_at_ms INTEGER NOT NULL,
  expires_at_ms INTEGER NOT NULL
);

-- Gateway-delivered signup activations have no coordinator outbox row. Existing
-- activations retain their coordinator-email delivery topology.
CREATE TABLE owner_activation_tokens_delivery (
  coordinator_id TEXT PRIMARY KEY,
  account_id TEXT UNIQUE NOT NULL,
  email_normalized TEXT UNIQUE NOT NULL,
  token_hash TEXT UNIQUE NOT NULL,
  outbox_id TEXT REFERENCES email_outbox(id),
  delivery TEXT NOT NULL CHECK(delivery IN ('coordinator-email','signup-gateway')),
  created_at_ms INTEGER NOT NULL,
  expires_at_ms INTEGER NOT NULL,
  accepted_at_ms INTEGER,
  revoked_at_ms INTEGER
);
INSERT INTO owner_activation_tokens_delivery (
  coordinator_id, account_id, email_normalized, token_hash, outbox_id, delivery,
  created_at_ms, expires_at_ms, accepted_at_ms, revoked_at_ms
)
SELECT coordinator_id, account_id, email_normalized, token_hash, outbox_id,
       'coordinator-email', created_at_ms, expires_at_ms, accepted_at_ms,
       revoked_at_ms
FROM owner_activation_tokens;
DROP TABLE owner_activation_tokens;
ALTER TABLE owner_activation_tokens_delivery RENAME TO owner_activation_tokens;
