-- Drop the last storage left over from structured agent mode (retired in
-- 0017). `agent_entries` was kept inert by 0016 so old transcript rows stayed
-- recoverable; nothing reads it since the structured session type, transcript
-- renderer, and agent RPC path were deleted, and the coding-agent status that
-- replaced them is volatile by design (no SQLite row at all).
--
-- The `agent_ui_*` tables predate 0001 in long-lived coordinator DBs and no
-- current migration creates them, so a fresh DB has none of them; the guarded
-- drops below clean up the DBs that still carry them.
DROP TABLE IF EXISTS agent_entries;

DROP TABLE IF EXISTS agent_ui_live_frame_staging;
DROP TABLE IF EXISTS agent_ui_snapshot_frame_staging;
DROP TABLE IF EXISTS agent_ui_tail_frames;
DROP TABLE IF EXISTS agent_ui_snapshot_frames;
DROP TABLE IF EXISTS agent_ui_snapshot_entries;
DROP TABLE IF EXISTS agent_ui_snapshot_staging;
DROP TABLE IF EXISTS agent_ui_entries;
DROP TABLE IF EXISTS agent_ui_sessions;
