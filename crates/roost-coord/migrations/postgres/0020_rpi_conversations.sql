DO $migration$
DECLARE
    backend_constraint TEXT;
BEGIN
    SELECT constraint_row.conname INTO STRICT backend_constraint
    FROM pg_constraint AS constraint_row
    JOIN pg_attribute AS attribute_row
      ON attribute_row.attrelid = constraint_row.conrelid
     AND attribute_row.attnum = ANY(constraint_row.conkey)
    WHERE constraint_row.conrelid = 'agent_conversations'::regclass
      AND constraint_row.contype = 'c'
      AND attribute_row.attname = 'backend'
      AND cardinality(constraint_row.conkey) = 1;
    EXECUTE format('ALTER TABLE agent_conversations DROP CONSTRAINT %I', backend_constraint);
END;
$migration$;
ALTER TABLE agent_conversations
    ADD CONSTRAINT agent_conversations_backend_check CHECK (backend IN ('native', 'omp', 'rpi'));

CREATE TABLE rpi_sessions (
    conversation_id TEXT PRIMARY KEY REFERENCES agent_conversations(id) ON DELETE CASCADE,
    actor_id TEXT,
    bound_worker_epoch TEXT,
    next_command_seq BIGINT NOT NULL DEFAULT 0 CHECK (next_command_seq >= 0),
    last_stop_seq BIGINT NOT NULL DEFAULT 0 CHECK (last_stop_seq >= 0),
    applied_event_seq BIGINT NOT NULL DEFAULT 0 CHECK (applied_event_seq >= 0),
    transcript_cursor BIGINT NOT NULL DEFAULT 0 CHECK (transcript_cursor >= 0),
    transcript_json TEXT NOT NULL,
    needs_resume BOOLEAN NOT NULL DEFAULT FALSE,
    deleted_ms BIGINT
);

CREATE TABLE rpi_commands (
    conversation_id TEXT NOT NULL REFERENCES agent_conversations(id) ON DELETE CASCADE,
    request_id TEXT NOT NULL,
    command_seq BIGINT NOT NULL CHECK (command_seq >= 0),
    kind TEXT NOT NULL CHECK (kind IN ('open', 'submit', 'abort', 'withdraw', 'configure', 'close')),
    payload_json TEXT NOT NULL,
    payload_fingerprint TEXT NOT NULL,
    delivery_state TEXT NOT NULL DEFAULT 'pending'
        CHECK (delivery_state IN ('pending', 'accepted', 'settled')),
    result_json TEXT,
    created_ms BIGINT NOT NULL,
    updated_ms BIGINT NOT NULL,
    PRIMARY KEY (conversation_id, request_id),
    UNIQUE (conversation_id, command_seq)
);

-- Request identity outlives the projection so a deleted creation cannot create a replacement.
CREATE TABLE rpi_creations (
    account_id TEXT NOT NULL REFERENCES accounts(id),
    request_id TEXT NOT NULL,
    conversation_id TEXT REFERENCES agent_conversations(id) ON DELETE SET NULL,
    original_conversation_id TEXT NOT NULL,
    payload_fingerprint TEXT NOT NULL,
    conversation_json TEXT NOT NULL,
    deleted_ms BIGINT,
    created_ms BIGINT NOT NULL,
    updated_ms BIGINT NOT NULL,
    PRIMARY KEY (account_id, request_id)
);
