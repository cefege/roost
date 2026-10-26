CREATE TABLE agent_schedules (
  conversation_id TEXT NOT NULL REFERENCES agent_conversations(id) ON DELETE CASCADE,
  id TEXT NOT NULL,
  text TEXT NOT NULL,
  due_ms BIGINT NOT NULL,
  every_ms BIGINT,
  fired INTEGER NOT NULL DEFAULT 0,
  created_ms BIGINT NOT NULL,
  PRIMARY KEY (conversation_id, id)
);

CREATE INDEX agent_schedules_due ON agent_schedules (due_ms);
