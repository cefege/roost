ALTER TABLE agent_conversations
    ADD COLUMN backend TEXT NOT NULL DEFAULT 'native' CHECK (backend IN ('native', 'omp'));

CREATE TABLE omp_sessions (
    conversation_id TEXT PRIMARY KEY REFERENCES agent_conversations(id) ON DELETE CASCADE,
    acp_session_id TEXT,
    current_model TEXT,
    current_thinking_level TEXT,
    applied_title TEXT,
    run_id TEXT,
    worker_epoch TEXT,
    applied_seq INTEGER NOT NULL DEFAULT 0,
    transcript_cursor INTEGER NOT NULL DEFAULT 0,
    transcript_json TEXT NOT NULL
);

CREATE TABLE omp_inputs (
    conversation_id TEXT NOT NULL REFERENCES agent_conversations(id) ON DELETE CASCADE,
    request_id TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('prompt', 'abort')),
    created_ms INTEGER NOT NULL,
    PRIMARY KEY (conversation_id, request_id)
);

CREATE TABLE omp_settings (
    scope TEXT NOT NULL CHECK (scope IN ('defaults', 'machine', 'folder')),
    worker_fp TEXT NOT NULL DEFAULT '',
    cwd TEXT NOT NULL DEFAULT '',
    model TEXT,
    thinking_level TEXT,
    updated_ms INTEGER NOT NULL,
    PRIMARY KEY (scope, worker_fp, cwd)
);

CREATE TABLE omp_worker_catalogs (
    worker_fp TEXT PRIMARY KEY,
    config_options_json TEXT NOT NULL,
    updated_ms INTEGER NOT NULL
);
