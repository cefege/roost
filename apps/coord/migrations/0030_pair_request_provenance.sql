ALTER TABLE pair_requests ADD COLUMN user_agent TEXT;
ALTER TABLE pair_requests ADD COLUMN client_browser TEXT;
ALTER TABLE pair_requests ADD COLUMN client_os TEXT;
ALTER TABLE pair_requests ADD COLUMN client_device_type TEXT;
ALTER TABLE pair_requests ADD COLUMN source_ip TEXT;
ALTER TABLE pair_requests ADD COLUMN country_code TEXT;
ALTER TABLE pair_requests ADD COLUMN region TEXT;
ALTER TABLE pair_requests ADD COLUMN city TEXT;
ALTER TABLE pair_requests ADD COLUMN edge_identity_provider TEXT;
ALTER TABLE pair_requests ADD COLUMN edge_identity TEXT;
ALTER TABLE pair_requests ADD COLUMN edge_identity_verified INTEGER NOT NULL DEFAULT 0;
ALTER TABLE pair_requests ADD COLUMN expires_at_ms INTEGER NOT NULL DEFAULT 0;

UPDATE pair_requests SET expires_at_ms = created_at_ms + 600000;

CREATE INDEX IF NOT EXISTS idx_pair_requests_status_expires
    ON pair_requests (status, expires_at_ms);

ALTER TABLE authorized_keys ADD COLUMN paired_from_ip TEXT;
ALTER TABLE authorized_keys ADD COLUMN paired_country TEXT;
ALTER TABLE authorized_keys ADD COLUMN paired_user_agent TEXT;
ALTER TABLE authorized_keys ADD COLUMN paired_edge_identity TEXT;
