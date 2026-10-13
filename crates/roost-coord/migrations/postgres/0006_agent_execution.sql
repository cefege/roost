-- Rows written before compare-and-set existed start at revision 1; 0 means
-- "not yet stored" to `save_conversation`.
ALTER TABLE agent_conversations ADD COLUMN revision BIGINT NOT NULL DEFAULT 1;
ALTER TABLE agent_conversations ADD COLUMN permission_profile TEXT NOT NULL DEFAULT 'full_access'
  CHECK (permission_profile IN ('full_access', 'approve_execution', 'read_only'));

CREATE TABLE agent_commands (
  conversation_id TEXT NOT NULL REFERENCES agent_conversations(id) ON DELETE CASCADE,
  request_id TEXT NOT NULL,
  kind TEXT NOT NULL,
  result_json TEXT,
  created_ms BIGINT NOT NULL,
  PRIMARY KEY (conversation_id, request_id)
);

CREATE TABLE agent_run_leases (
  conversation_id TEXT PRIMARY KEY REFERENCES agent_conversations(id) ON DELETE CASCADE,
  owner_instance TEXT NOT NULL,
  epoch BIGINT NOT NULL,
  expires_ms BIGINT NOT NULL
);

CREATE TABLE agent_chat_events (
  conversation_id TEXT NOT NULL REFERENCES agent_conversations(id) ON DELETE CASCADE,
  cursor BIGINT NOT NULL,
  events_json TEXT NOT NULL,
  created_ms BIGINT NOT NULL,
  PRIMARY KEY (conversation_id, cursor)
);

CREATE TABLE agent_tool_calls (
  call_id TEXT PRIMARY KEY,
  conversation_id TEXT NOT NULL REFERENCES agent_conversations(id) ON DELETE CASCADE,
  worker_fp TEXT NOT NULL,
  lease_epoch BIGINT NOT NULL,
  state TEXT NOT NULL CHECK (state IN ('admitted', 'dispatched', 'completed', 'unknown', 'cancelled')),
  result_json TEXT,
  created_ms BIGINT NOT NULL,
  updated_ms BIGINT NOT NULL
);
CREATE INDEX agent_tool_calls_conversation ON agent_tool_calls (conversation_id, state);
