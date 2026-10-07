-- Durable OMP conversation references are no longer written or read; a stored
-- one would fail decoding in every backfill. The two sessions.agent_reference_*
-- columns stay (always NULL): SQLite cannot drop a column a CHECK names.
DELETE FROM events WHERE kind = 'agent_reference';
UPDATE sessions SET agent_reference_json = NULL, agent_reference_client_seq = NULL;
