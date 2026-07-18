// Per-folder activity derived from the session store — how many terminals
// and running agents live inside a folder or its subtree. Pure client-side
// join of the filesystem listing (BrowsePage) with the session array.
// Zero RPCs: all data is already in memory.
//
// Cumulative: for folder /Users/you/Code, counts sessions in
// /Users/you/Code AND /Users/you/Code/roost AND .../idea/apps/web —
// every descendant. The user asked for this ("in total, this many agents").
//
// Called by: BrowsePage.tsx (the "+" folder browser).

import type { Session } from "@roost/shared/wire";

export interface FolderActivity {
  /** Total sessions whose cwd is this folder or any descendant. */
  terminals: number;
  /** Of those, claude sessions with status running or needs-input. */
  agentsRunning: number;
  /** Of those, claude sessions specifically blocked awaiting user input. */
  needsInput: number;
}

export const EMPTY_ACTIVITY: FolderActivity = {
  terminals: 0,
  agentsRunning: 0,
  needsInput: 0,
};

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
  const serverSessions = sessions.filter((s) => String(s.worker_fp) === workerFp);
  const out = new Map<string, FolderActivity>();

  for (const fp of folderPaths) {
    const base = stripTrailingSlash(fp);
    // For root "/", prefix is "/" and every path matches. For everything else,
    // a session at /a/b counts toward /a if its cwd is /a OR starts with /a/.
    const prefix = base.endsWith("/") ? base : base + "/";

    let terminals = 0;
    let agentsRunning = 0;
    let needsInput = 0;

    for (const s of serverSessions) {
      const cwd = stripTrailingSlash(s.cwd);
      if (cwd !== base && !cwd.startsWith(prefix)) continue;
      terminals++;
      const st = s.agent?.status;
      if (st === "needs-input") {
        agentsRunning++;
        needsInput++;
      } else if (st === "running") {
        agentsRunning++;
      }
    }

    if (terminals > 0) {
      out.set(fp, { terminals, agentsRunning, needsInput });
    }
  }

  return out;
}

function stripTrailingSlash(p: string): string {
  if (p.length <= 1) return p; // preserve "/" and "~"
  return p.replace(/\/+$/, "");
}
