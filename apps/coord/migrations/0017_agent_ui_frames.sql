-- Durable browser-facing OMP HostFrame replica.
-- Completed snapshot trains atomically replace agent_ui_entries; incomplete
-- trains remain isolated in the staging tables and never affect replay.
CREATE TABLE IF NOT EXISTS agent_ui_sessions (
  session_id   TEXT PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE,
  snapshot_id  TEXT,
  welcome_json TEXT,
  state_json   TEXT,
  agents_json  TEXT,
  last_revision INTEGER NOT NULL DEFAULT 0,
  updated_at   INTEGER NOT NULL
) WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS agent_ui_entries (
  session_id  TEXT    NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
  entry_id    TEXT    NOT NULL,
  ordinal     INTEGER NOT NULL,
  entry_json  TEXT    NOT NULL,
  updated_at  INTEGER NOT NULL,
  PRIMARY KEY (session_id, entry_id),
  UNIQUE (session_id, ordinal)
) WITHOUT ROWID;


CREATE TABLE IF NOT EXISTS agent_ui_snapshot_staging (
  session_id       TEXT PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE,
  snapshot_id      TEXT    NOT NULL,
  baseline_revision INTEGER NOT NULL,
  welcome_json     TEXT    NOT NULL,
  state_json       TEXT    NOT NULL,
  agents_json      TEXT    NOT NULL,
  expected_entries INTEGER NOT NULL,
  staged_bytes     INTEGER NOT NULL DEFAULT 0,
  staged_frame_bytes INTEGER NOT NULL DEFAULT 0,
  staged_live_bytes INTEGER NOT NULL DEFAULT 0,
  created_at       INTEGER NOT NULL,
  updated_at       INTEGER NOT NULL
) WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS agent_ui_snapshot_entries (
  session_id  TEXT    NOT NULL REFERENCES agent_ui_snapshot_staging(session_id) ON DELETE CASCADE,
  entry_id    TEXT    NOT NULL,
  ordinal     INTEGER NOT NULL,
  entry_json  TEXT    NOT NULL,
  entry_bytes INTEGER NOT NULL,
  PRIMARY KEY (session_id, entry_id),
  UNIQUE (session_id, ordinal)
) WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS agent_ui_snapshot_frames (
  session_id TEXT    NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
  ordinal    INTEGER NOT NULL,
  revision   INTEGER NOT NULL,
  frame_json TEXT    NOT NULL,
  PRIMARY KEY (session_id, ordinal),
  UNIQUE (session_id, revision)
) WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS agent_ui_tail_frames (
  session_id TEXT    NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
  revision   INTEGER NOT NULL,
  frame_json TEXT    NOT NULL,
  PRIMARY KEY (session_id, revision)
) WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS agent_ui_snapshot_frame_staging (
  session_id TEXT    NOT NULL REFERENCES agent_ui_snapshot_staging(session_id) ON DELETE CASCADE,
  ordinal    INTEGER NOT NULL,
  frame_json TEXT    NOT NULL,
  PRIMARY KEY (session_id, ordinal)
) WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS agent_ui_live_frame_staging (
  session_id TEXT    NOT NULL REFERENCES agent_ui_snapshot_staging(session_id) ON DELETE CASCADE,
  ordinal    INTEGER NOT NULL,
  frame_json TEXT    NOT NULL,
  PRIMARY KEY (session_id, ordinal)
) WITHOUT ROWID;
