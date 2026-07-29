-- agent_entries: durable agent-session transcript, one row per (session, seq).
-- Upserted on the coord relay path; the worker's ring is now only a live
-- coalescer. Paged newest-first by SessionsGetAgentEntries.
CREATE TABLE IF NOT EXISTS agent_entries (
  session_id TEXT    NOT NULL,
  seq        INTEGER NOT NULL,
  ts         INTEGER NOT NULL,
  entry_json TEXT    NOT NULL,
  PRIMARY KEY (session_id, seq)
) WITHOUT ROWID;
