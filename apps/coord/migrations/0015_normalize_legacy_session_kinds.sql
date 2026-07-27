-- Legacy workers persisted agent/claude labels for PTY sessions. The v2
-- terminal transport supports one PTY kind: shell. Normalize open rows before
-- a worker reconnect emits an opened snapshot, so coord can decode and recover
-- every surviving keeper channel.
UPDATE sessions
SET kind = 'shell'
WHERE status = 'open'
  AND kind IN ('agent', 'claude');
