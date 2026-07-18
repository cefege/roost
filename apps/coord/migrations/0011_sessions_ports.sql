-- Session listening ports. JSON int[] of TCP ports the session's process tree
-- is LISTENing on, detected on the worker via ps pid-tree + lsof and pushed via
-- the `ports` SessionEvent. Feeds :5174 folder-row chips
-- (apps/web/src/components/sidebar/FolderList.tsx). null = nothing listening.

ALTER TABLE sessions ADD COLUMN ports_json TEXT;
