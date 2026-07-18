-- Session git branch. Local branch of the session's cwd, resolved on the
-- worker host and pushed via the `git` SessionEvent. Feeds the
-- folder-row subtitle (apps/web/src/components/sidebar/FolderList.tsx).
-- nullable: sessions opened before the field stay valid; null = not a repo
-- / not yet resolved.

ALTER TABLE sessions ADD COLUMN git_branch TEXT;
