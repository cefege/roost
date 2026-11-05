CREATE TABLE omp_inputs_new (
    conversation_id TEXT NOT NULL REFERENCES agent_conversations(id) ON DELETE CASCADE,
    request_id TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('prompt', 'abort', 'respond', 'ui')),
    created_ms INTEGER NOT NULL,
    PRIMARY KEY (conversation_id, request_id)
);
INSERT INTO omp_inputs_new(conversation_id, request_id, kind, created_ms)
SELECT conversation_id, request_id, kind, created_ms FROM omp_inputs;
DROP TABLE omp_inputs;
ALTER TABLE omp_inputs_new RENAME TO omp_inputs;
