-- Replace only the inline discriminator constraint: child rows and their foreign keys stay intact.
ALTER TABLE agent_conversations
    ADD COLUMN backend_next TEXT NOT NULL DEFAULT 'native'
        CHECK (backend_next IN ('native', 'omp', 'rpi'));
UPDATE agent_conversations SET backend_next = backend;
ALTER TABLE agent_conversations DROP COLUMN backend;
ALTER TABLE agent_conversations RENAME COLUMN backend_next TO backend;

CREATE TABLE rpi_sessions (
    conversation_id TEXT PRIMARY KEY REFERENCES agent_conversations(id) ON DELETE CASCADE,
    actor_id TEXT,
    bound_worker_epoch TEXT,
    next_command_seq BIGINT NOT NULL DEFAULT 0 CHECK (next_command_seq >= 0),
    last_stop_seq BIGINT NOT NULL DEFAULT 0 CHECK (last_stop_seq >= 0),
    applied_event_seq BIGINT NOT NULL DEFAULT 0 CHECK (applied_event_seq >= 0),
    transcript_cursor BIGINT NOT NULL DEFAULT 0 CHECK (transcript_cursor >= 0),
    transcript_json TEXT NOT NULL,
    needs_resume INTEGER NOT NULL DEFAULT 0 CHECK (needs_resume IN (0, 1)),
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
