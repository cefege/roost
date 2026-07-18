// Shared session-list urgency ranking for the sidebar (FolderList) and
// ClaudeFeedGroup, so every consumer ranks "urgency" identically.
//
// Pure functions only — no store reads, no reactivity. Callers pass the
// already-collected session array.

import type { Session } from "@roost/shared/wire";

// Higher number = higher up in the list. Unknown/missing status falls
// into the lowest bucket so it doesn't push genuine activity down.
//   needs-input(4) — claude waiting for you (most urgent)
//   running(3) / running-workflow(3, blocked-on-subtask but still active)
//   idle(2) — between turns
//   done(1) — finished
const STAGE_PRIORITY: Record<string, number> = {
  "needs-input": 4,
  "running": 3,
  "running-workflow": 3,
  "idle": 2,
  "done": 1,
};

/** Rank a resolved agent-status string. Unknown/empty → 0 (lowest). */
export function stageRank(status: string | null | undefined): number {
  if (!status) return 0;
  return STAGE_PRIORITY[status] ?? 0;
}

/** ClaudeFeedGroup variant: non-claude sessions sort below all claude
 * sessions (return -1) so the claude-only feed stays claude-first. */
export function claudeStageOf(s: Session): number {
  if (s.kind !== "claude") return -1;
  return stageRank(s.agent?.status);
}
