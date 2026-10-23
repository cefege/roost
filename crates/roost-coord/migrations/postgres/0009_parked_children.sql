-- A child conversation whose run settled (completed or yielded) is parked until
-- `AgentChatRevive` clears the marker. Every other conversation stays false.
ALTER TABLE agent_conversations ADD COLUMN parked BOOLEAN NOT NULL DEFAULT FALSE;
