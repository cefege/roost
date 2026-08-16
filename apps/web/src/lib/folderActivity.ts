// Per-folder terminal-session counts. Zero RPCs: all data is already in memory.
//
// Cumulative: for folder /Users/you/Code, counts sessions in
// /Users/you/Code AND /Users/you/Code/roost AND .../idea/apps/web —
// every descendant.
//
// Called by: BrowsePage.tsx (the "+" folder browser).

import type { Session } from "@roost/shared/wire";
import { workerPathIdentity } from "./nativePath.ts";

export interface FolderActivity {
  /** Total sessions whose cwd is this folder or any descendant. */
  terminals: number;
}

/**
 * For a set of folder paths on a given server, compute how many sessions
 * live inside each folder (directly or in any subfolder — cumulative).
 *
 * Returns a Map keyed by the exact input path string → FolderActivity.
 * Paths with zero sessions are omitted (callers check `.has(path)`).
 *
 * Cost: O(serverSessions × folderPaths) string comparisons — sub-ms
 * for typical scale (tens of sessions, ~50 visible folders).
 */
export function computeFolderActivity(
  sessions: Session[],
  workerFp: string,
  folderPaths: string[],
): Map<string, FolderActivity> {
  const serverSessions = sessions.filter(
    (s) => s.kind === "shell" && String(s.worker_fp) === workerFp,
  );
  const out = new Map<string, FolderActivity>();

  for (const fp of folderPaths) {
    const base = workerPathIdentity(workerFp, fp);
    // Identity keys preserve POSIX case and case-fold Windows while retaining
    // canonical forward-slash boundaries. A separator suffix prevents sibling
    // prefixes (`C:/work` vs `C:/worker`) from colliding.
    const prefix = base.endsWith("/") ? base : `${base}/`;

    let terminals = 0;

    for (const s of serverSessions) {
      const cwd = workerPathIdentity(workerFp, s.cwd);
      if (cwd !== base && !cwd.startsWith(prefix)) continue;
      terminals++;
    }

    if (terminals > 0) {
      out.set(fp, { terminals });
    }
  }

  return out;
}
