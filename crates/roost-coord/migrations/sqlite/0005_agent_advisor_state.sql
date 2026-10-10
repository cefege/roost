CREATE TABLE agent_advisor_state (
  conversation_id TEXT PRIMARY KEY REFERENCES agent_conversations(id) ON DELETE CASCADE,
  state_json TEXT NOT NULL,
  updated_ms INTEGER NOT NULL
);
