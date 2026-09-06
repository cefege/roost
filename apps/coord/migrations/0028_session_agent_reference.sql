-- Private opaque agent-conversation recovery projection. The JSON is never part
-- of public Session state; the sequence advances on set, replace, and clear.

ALTER TABLE sessions ADD COLUMN agent_reference_json TEXT;
ALTER TABLE sessions ADD COLUMN agent_reference_client_seq INTEGER
  CHECK (
    (
      agent_reference_json IS NULL
      AND agent_reference_client_seq IS NULL
    )
    OR (
      typeof(agent_reference_client_seq) = 'integer'
      AND agent_reference_client_seq > 0
      AND agent_reference_client_seq <= 9007199254740991
    )
  );
