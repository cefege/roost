-- 0015 already normalized every open legacy agent/claude PTY to shell.
-- Any row still open with kind='agent' is therefore a structured session whose
-- OMP child disappears in this cutover. Retire it before normalizing history.
UPDATE sessions
   SET status = 'closed',
       closed_at = CAST(strftime('%s','now') AS INTEGER) * 1000
 WHERE status = 'open'
   AND kind = 'agent';

UPDATE sessions SET kind = 'shell' WHERE kind <> 'shell';
