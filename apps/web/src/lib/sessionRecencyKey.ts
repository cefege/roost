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

/** Agent-feed variant: a session with no agent identity (plain shell) sorts
 *  below every agent one (return -1) so the feed stays agent-first. Both agent
 *  kinds qualify — a claude PTY and a `kind:"agent"` omp session each carry
 *  AgentState. */
export function claudeStageOf(s: Session): number {
  if (s.kind === "shell") return -1;
  return stageRank(s.agent?.status);
}
