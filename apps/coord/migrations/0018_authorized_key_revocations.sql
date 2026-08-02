CREATE TABLE authorized_key_revocations (
  fingerprint   TEXT PRIMARY KEY,
  revoked_at_ms INTEGER NOT NULL,
  revoked_by_fp TEXT NOT NULL,
  reason        TEXT NOT NULL
);

ALTER TABLE bootstrap_tokens ADD COLUMN minted_by_fp TEXT;

CREATE TRIGGER authorized_keys_reject_revoked_insert
BEFORE INSERT ON authorized_keys
WHEN EXISTS (
  SELECT 1 FROM authorized_key_revocations
  WHERE fingerprint = NEW.fingerprint
)
BEGIN
  SELECT RAISE(ABORT, 'authorized key revoked');
END;

CREATE TRIGGER bootstrap_tokens_reject_revoked_minter
BEFORE INSERT ON bootstrap_tokens
WHEN NEW.minted_by_fp IS NOT NULL AND EXISTS (
  SELECT 1 FROM authorized_key_revocations
  WHERE fingerprint = NEW.minted_by_fp
)
BEGIN
  SELECT RAISE(ABORT, 'bootstrap minter revoked');
END;
