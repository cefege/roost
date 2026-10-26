-- Account-scoped browser state shared across an operator's devices: one row per
-- account and allowlisted key (`roost-protocol::client_state`). `updated_ms` is
-- the writing browser's wall clock and decides last-writer-wins on upsert.
-- The Postgres translation of `migrations/sqlite/0015_client_state.sql`.
CREATE TABLE client_state (
  account_id TEXT NOT NULL REFERENCES accounts(id),
  key TEXT NOT NULL,
  value TEXT NOT NULL,
  updated_ms BIGINT NOT NULL,
  PRIMARY KEY (account_id, key)
);
