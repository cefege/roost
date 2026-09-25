// Resolve + watch the local git branch of a session's cwd on the worker host.
// Only the worker can read git (the browser can't shell out), so branch is
// computed here and pushed to coord/SPA via the `git` SessionEvent, feeding
// the cell-grid folder-row subtitle (apps/web/src/components/sidebar/FolderList.tsx).
// Called by session-manager.ts (_startGitBranch) on spawn/resume/cwd-change.

import { watch, type FSWatcher } from "node:fs";
import { resolve } from "node:path";

async function runGit(cwd: string, args: string[]): Promise<string | null> {
  try {
    const proc = Bun.spawn(["git", ...args], { cwd, stdout: "pipe", stderr: "ignore" });
    const out = (await new Response(proc.stdout).text()).trim();
    await proc.exited;
    return proc.exitCode === 0 && out.length > 0 ? out : null;
  } catch {
    return null; // git missing / cwd gone → treat as "not a repo"
  }
}

/** Current branch of `cwd`, or null if it isn't a git repo. Detached HEAD →
 *  `@<short-sha>` so the row shows something stable instead of empty. */
export async function readGitBranch(cwd: string): Promise<string | null> {
  const ref = await runGit(cwd, ["rev-parse", "--abbrev-ref", "HEAD"]);
  if (!ref) return null;
  if (ref !== "HEAD") return ref;
  const sha = await runGit(cwd, ["rev-parse", "--short", "HEAD"]);
  return sha ? `@${sha}` : null;
}

/** GitHub "owner/repo" of the origin remote, or null (no origin / non-github).
 *  Parses both ssh (`git@github.com:owner/repo.git`) and https forms. Stable —
 *  resolved once per session, not watched. */
export async function readGitRemote(cwd: string): Promise<string | null> {
  const url = await runGit(cwd, ["remote", "get-url", "origin"]);
  if (!url) return null;
  const m = url.match(/github\.com[:/]([^/]+)\/([^/]+?)(?:\.git)?\/?$/);
  return m ? `${m[1]}/${m[2]}` : null;
}

/** Watch the repo HEAD for branch switches; fires `onChange(branch)` (debounced)
 *  whenever HEAD moves. Returns a disposer — a no-op when `cwd` isn't a repo.
 *  Uses `git rev-parse --git-path HEAD` so linked worktrees (where `.git` is a
 *  file) resolve to the real HEAD file. */
export async function watchGitBranch(
  cwd: string,
  onChange: (branch: string | null) => void,
): Promise<() => void> {
  const headRel = await runGit(cwd, ["rev-parse", "--git-path", "HEAD"]);
  if (!headRel) return () => {}; // not a repo
  const headPath = resolve(cwd, headRel);
  let watcher: FSWatcher | undefined;
  let timer: ReturnType<typeof setTimeout> | null = null;
  try {
    watcher = watch(headPath, () => {
      if (timer) clearTimeout(timer);
      timer = setTimeout(() => { void readGitBranch(cwd).then(onChange); }, 150);
    });
  } catch {
    return () => {};
  }
  return () => { if (timer) clearTimeout(timer); watcher?.close(); };
}
