-- The session tree: every entry records the entry it hangs from, and each
-- conversation names the leaf the next append hangs from. `tree_revision` is
-- the compare-and-set token a navigation checks. `journal_format` 1 is a
-- journal written before parents existed; the backfill rewrites it to 2 once.
ALTER TABLE agent_entries ADD COLUMN parent_seq BIGINT;
CREATE INDEX agent_entries_parent ON agent_entries (conversation_id, parent_seq);

ALTER TABLE agent_conversations ADD COLUMN leaf_seq BIGINT;
ALTER TABLE agent_conversations ADD COLUMN journal_format BIGINT NOT NULL DEFAULT 1;
ALTER TABLE agent_conversations ADD COLUMN tree_revision BIGINT NOT NULL DEFAULT 0;
ALTER TABLE agent_conversations ADD COLUMN branched_from_conversation_id TEXT;
ALTER TABLE agent_conversations ADD COLUMN branched_from_seq BIGINT;
ALTER TABLE agent_conversations ADD COLUMN additional_dirs TEXT NOT NULL DEFAULT '[]';
ALTER TABLE agent_conversations ADD COLUMN pinned BOOLEAN NOT NULL DEFAULT FALSE;
ALTER TABLE agent_conversations ADD COLUMN provider_session_key TEXT;
