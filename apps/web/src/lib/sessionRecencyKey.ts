// Pure stage-ranking utility shared by sidebar consumers.

// Higher number sorts first: approval, active work, then idle/completed.
const STAGE_PRIORITY: Record<string, number> = {
  "needs-input": 4,
  "running": 3,
  "idle": 2,
  "done": 1,
};

/** Rank a resolved OMP state. Unknown or empty is lowest. */
export function stageRank(status: string | null | undefined): number {
  return status ? (STAGE_PRIORITY[status] ?? 0) : 0;
}
