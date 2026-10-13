-- Rows written before compare-and-set existed start at revision 1; 0 means
-- "not yet stored" to `save_conversation`.
ALTER TABLE agent_conversations ADD COLUMN revision INTEGER NOT NULL DEFAULT 1;
ALTER TABLE agent_conversations ADD COLUMN permission_profile TEXT NOT NULL DEFAULT 'full_access'
  CHECK (permission_profile IN ('full_access', 'approve_execution', 'read_only'));

CREATE TABLE agent_commands (
  conversation_id TEXT NOT NULL REFERENCES agent_conversations(id) ON DELETE CASCADE,
  request_id TEXT NOT NULL,
  kind TEXT NOT NULL,
  result_json TEXT,
  created_ms INTEGER NOT NULL,
  PRIMARY KEY (conversation_id, request_id)
);

CREATE TABLE agent_run_leases (
  conversation_id TEXT PRIMARY KEY REFERENCES agent_conversations(id) ON DELETE CASCADE,
  owner_instance TEXT NOT NULL,
  epoch INTEGER NOT NULL,
  expires_ms INTEGER NOT NULL
);

-- Every transcript event batch, numbered per conversation. `run_start` marks
-- the batch that began a run: a snapshot folds the live events from there on.
CREATE TABLE agent_chat_events (
  conversation_id TEXT NOT NULL REFERENCES agent_conversations(id) ON DELETE CASCADE,
  cursor INTEGER NOT NULL,
  events_json TEXT NOT NULL,
  run_start INTEGER NOT NULL DEFAULT 0 CHECK (run_start IN (0, 1)),
  created_ms INTEGER NOT NULL,
  PRIMARY KEY (conversation_id, cursor)
);

-- Leases on work that is not a conversation's run: a provider sign-in's
-- device-code poller, the schedule sweeper.
CREATE TABLE coord_leases (
  name TEXT PRIMARY KEY,
  owner_instance TEXT NOT NULL,
  epoch INTEGER NOT NULL,
  expires_ms INTEGER NOT NULL
);

-- Live coordinator replicas and the address their peers reach them on.
CREATE TABLE coord_instances (
  instance_id TEXT PRIMARY KEY,
  internal_url TEXT NOT NULL,
  heartbeat_ms INTEGER NOT NULL
);

-- Provider sign-ins in progress, so any replica can answer a poll.
CREATE TABLE agent_logins (
  id TEXT PRIMARY KEY,
  provider TEXT NOT NULL,
  state_json TEXT NOT NULL,
  expires_ms INTEGER NOT NULL
);

CREATE TABLE agent_tool_calls (
  call_id TEXT PRIMARY KEY,
  conversation_id TEXT NOT NULL REFERENCES agent_conversations(id) ON DELETE CASCADE,
  worker_fp TEXT NOT NULL,
  lease_epoch INTEGER NOT NULL,
  state TEXT NOT NULL CHECK (state IN ('admitted', 'dispatched', 'completed', 'unknown', 'cancelled')),
  result_json TEXT,
  created_ms INTEGER NOT NULL,
  updated_ms INTEGER NOT NULL
);
CREATE INDEX agent_tool_calls_conversation ON agent_tool_calls (conversation_id, state);
