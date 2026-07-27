// Per-folder activity derived from terminal sessions and OMP transcript state.
// It counts terminals in a folder or subtree and derives active/blocked agents
// through the shared OMP attention projection.
// Zero RPCs: all data is already in memory.
//
// Cumulative: for folder /Users/you/Code, counts sessions in
// /Users/you/Code AND /Users/you/Code/roost AND .../idea/apps/web —
// every descendant.
//
// Called by: BrowsePage.tsx (the "+" folder browser).

import type { AgentStatus, Session } from "@roost/shared/wire";
import { liveStatus } from "./attention.ts";

export interface FolderActivity {
  /** Total sessions whose cwd is this folder or any descendant. */
  terminals: number;
  /** Of those, OMP sessions currently running or awaiting user input. */
  agentsRunning: number;
  /** Of those, OMP sessions specifically blocked awaiting user input. */
  needsInput: number;
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
  statusOf: (session: Session) => AgentStatus | undefined = liveStatus,
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
      const st = statusOf(s);
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
