-- Session user-rename. Sticky override of the auto-derived title (OSC/tool/
-- cwd) resolved in apps/web/src/lib/sessionTitle.ts. Set via the `renamed`
-- SessionEvent (coord SessionsRename RPC). nullable: sessions opened before
-- the field stay valid; null = no override.

ALTER TABLE sessions ADD COLUMN custom_title TEXT;
