-- Session GitHub PR status for git_branch, resolved on the worker via
-- `gh pr list --head <branch>` and pushed via the `pr` SessionEvent. Feeds the
-- #123 ✓ folder-row badge (apps/web/src/components/sidebar/FolderList.tsx).
-- All nullable: pr_number null = no open PR for the branch.

ALTER TABLE sessions ADD COLUMN pr_number INTEGER;
ALTER TABLE sessions ADD COLUMN pr_state TEXT;
ALTER TABLE sessions ADD COLUMN pr_checks TEXT;
ALTER TABLE sessions ADD COLUMN pr_url TEXT;
