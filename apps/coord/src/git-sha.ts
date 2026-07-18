// Coord's own git_sha — reported by auth.coordIdentity + misc.health.
// Drives the DriftBadge: workers compare their stamped sha to this
// value, lit when they differ.
//
// History: install.sh used to stamp HEAD into ROOST_GIT_SHA at install
// time and that's all coord ever read — so the reported sha never
// updated, even after dozens of commits + coord restarts. Workers
// deployed at current HEAD then looked "drifted" forever.
//
// Resolution order:
//   1. Live `git rev-parse HEAD` against cwd (the repo root under
//      launchd). Updates with every commit, no install/replist
//      cycle needed.
//   2. ROOST_GIT_SHA env (legacy path) when git isn't available
//      (e.g. coord running outside the repo / git binary missing).
//   3. "dev" sentinel — surfaces as drift on every worker so the
//      user notices something is off.
//
// Captured once at module load; coord restart picks up new commits.

function _resolve(): string {
  try {
    const r = Bun.spawnSync(["git", "rev-parse", "HEAD"]);
    if (r.exitCode === 0) {
      const sha = r.stdout.toString().trim();
      if (sha.length > 0) return sha;
    }
  } catch { /* git missing or cwd not a repo */ }
  return process.env.ROOST_GIT_SHA ?? "dev";
}

export const COORD_GIT_SHA = _resolve();
