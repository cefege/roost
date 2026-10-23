ALTER TABLE agent_advisor_state
  ADD COLUMN roster_slug TEXT NOT NULL DEFAULT '';
ALTER TABLE agent_advisor_state
  DROP CONSTRAINT agent_advisor_state_pkey;
ALTER TABLE agent_advisor_state
  ADD CONSTRAINT agent_advisor_state_pkey PRIMARY KEY (conversation_id, roster_slug);
