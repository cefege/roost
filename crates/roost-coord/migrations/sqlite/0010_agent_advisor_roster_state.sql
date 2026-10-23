CREATE TABLE agent_advisor_state_roster (
  conversation_id TEXT NOT NULL REFERENCES agent_conversations(id) ON DELETE CASCADE,
  roster_slug TEXT NOT NULL DEFAULT '',
  state_json TEXT NOT NULL,
  updated_ms INTEGER NOT NULL,
  PRIMARY KEY (conversation_id, roster_slug)
);
INSERT INTO agent_advisor_state_roster(conversation_id,roster_slug,state_json,updated_ms)
SELECT conversation_id,'',state_json,updated_ms FROM agent_advisor_state;
DROP TABLE agent_advisor_state;
ALTER TABLE agent_advisor_state_roster RENAME TO agent_advisor_state;
