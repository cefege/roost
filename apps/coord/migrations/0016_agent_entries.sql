-- Historical structured-session transcript rows. The terminal-only runtime
-- keeps this table inert but intact so existing user history stays recoverable.
CREATE TABLE IF NOT EXISTS agent_entries (
  session_id TEXT    NOT NULL,
  seq        INTEGER NOT NULL,
  ts         INTEGER NOT NULL,
  entry_json TEXT    NOT NULL,
  PRIMARY KEY (session_id, seq)
) WITHOUT ROWID;
