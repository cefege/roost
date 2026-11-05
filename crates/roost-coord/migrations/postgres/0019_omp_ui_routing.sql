ALTER TABLE omp_sessions
    ADD COLUMN omp_ui_transport_id TEXT NOT NULL DEFAULT '';

ALTER TABLE omp_inputs
    ADD COLUMN fingerprint TEXT NOT NULL DEFAULT '',
    ADD COLUMN client_id TEXT NOT NULL DEFAULT '',
    ADD COLUMN tab_id TEXT NOT NULL DEFAULT '',
    ADD COLUMN view_id TEXT NOT NULL DEFAULT '',
    ADD COLUMN transport_id TEXT NOT NULL DEFAULT '',
    ADD COLUMN worker_fp TEXT NOT NULL DEFAULT '',
    ADD COLUMN receipt BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN expires_ms BIGINT NOT NULL DEFAULT 0;

-- Ephemeral effect routing: identity and kind only, never the effect value.
CREATE TABLE omp_host_services (
    service_id TEXT PRIMARY KEY,
    conversation_id TEXT NOT NULL REFERENCES agent_conversations(id) ON DELETE CASCADE,
    origin_request_id TEXT NOT NULL,
    fingerprint TEXT NOT NULL,
    client_id TEXT NOT NULL,
    tab_id TEXT NOT NULL,
    view_id TEXT NOT NULL,
    transport_id TEXT NOT NULL,
    worker_fp TEXT NOT NULL,
    kind TEXT NOT NULL,
    created_ms BIGINT NOT NULL,
    expires_ms BIGINT NOT NULL
);

CREATE INDEX omp_host_services_conversation
    ON omp_host_services(conversation_id, expires_ms);
