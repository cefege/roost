CREATE TABLE clipboard_entries (
  id TEXT PRIMARY KEY NOT NULL,
  text TEXT NOT NULL,
  source_session_id TEXT,
  source_worker_fp TEXT,
  source_kind TEXT NOT NULL CHECK (source_kind IN ('osc52', 'selection', 'command_output')),
  created_at_ms BIGINT NOT NULL
);

CREATE INDEX clipboard_entries_created_idx
  ON clipboard_entries(created_at_ms DESC, id DESC);
