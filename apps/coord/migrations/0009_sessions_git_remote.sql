-- Session GitHub remote. "owner/repo" of the session's origin remote (github.com
-- only), resolved on the worker host and pushed via the `git` SessionEvent.
-- Feeds clickable owner/repo#123, bare #123, and commit-SHA terminal links
-- (apps/web/src/components/terminal-links.ts). nullable: null = no github origin.

ALTER TABLE sessions ADD COLUMN git_remote TEXT;
