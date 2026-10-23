-- Keep deployed conversation-keyed writers valid during rolling upgrades.
CREATE UNIQUE INDEX agent_advisor_state_conversation_key
  ON agent_advisor_state(conversation_id);
