-- Legacy pending rows lack requester tokens, so v1 expires them before verifier fields exist.
UPDATE pair_requests
SET status = 'expired',
    decided_at_ms = COALESCE(
        decided_at_ms,
        CAST(strftime('%s', 'now') AS INTEGER) * 1000
    )
WHERE status = 'pending';

ALTER TABLE pair_requests ADD COLUMN ceremony_version INTEGER NOT NULL DEFAULT 0;
ALTER TABLE pair_requests ADD COLUMN requester_token_hash TEXT NOT NULL DEFAULT '';
ALTER TABLE pair_requests ADD COLUMN verification_code_hash TEXT;
ALTER TABLE pair_requests ADD COLUMN verification_attempts INTEGER NOT NULL DEFAULT 0;
ALTER TABLE pair_requests ADD COLUMN approved_by_fp TEXT;
ALTER TABLE pair_requests ADD COLUMN approved_account_id TEXT;
