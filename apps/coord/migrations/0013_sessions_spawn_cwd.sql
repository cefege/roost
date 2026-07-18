-- Immutable spawn folder. The folder a session was SPAWNED in, captured once
-- from the `opened` event and never updated by `cwd` drift (OSC7 cd tracking).
-- Backs the stable /t/:workerFp/*folderPath terminal URL
-- (apps/web/src/store/selectors.ts::resolveSessionByFolder). null = pre-migration.
-- Backfill existing open rows from their current cwd (best-effort — a session
-- that has already cd'd away loses precision, but new spawns are exact).

ALTER TABLE sessions ADD COLUMN spawn_cwd TEXT;
UPDATE sessions SET spawn_cwd = cwd WHERE spawn_cwd IS NULL;
