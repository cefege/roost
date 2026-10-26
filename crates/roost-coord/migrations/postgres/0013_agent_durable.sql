CREATE TABLE agent_durable_batches (
  conversation_id TEXT NOT NULL REFERENCES agent_conversations(id) ON DELETE CASCADE,
  sequence BIGINT NOT NULL,
  writes_json TEXT NOT NULL,
  entry_sequences_json TEXT NOT NULL,
  created_ms BIGINT NOT NULL,
  PRIMARY KEY (conversation_id, sequence)
);

CREATE TABLE agent_durable_ids (
  conversation_id TEXT PRIMARY KEY REFERENCES agent_conversations(id) ON DELETE CASCADE,
  next_id BIGINT NOT NULL CHECK (next_id >= 1 AND next_id <= 9007199254740991)
);
