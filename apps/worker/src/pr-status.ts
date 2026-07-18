// Resolve a session branch's GitHub PR status via the `gh` CLI on the worker
// host (the browser can't shell out; only the worker can reach gh + the repo).
// Pushed to coord/SPA via the `pr` SessionEvent → feeds the #123 ✓
// folder-row badge (apps/web/src/components/sidebar/FolderList.tsx).
// Called by session-manager.ts (_startPrStatus) on spawn/branch-change + 90s poll.
//
// Every failure path (gh missing, not authed, no PR, network) resolves to null —
// the badge just doesn't render. Never throws. Mirrors git-branch.ts.

// gh lives in /opt/homebrew/bin (Apple Silicon) or /usr/local/bin (Intel),
// neither on the worker LaunchAgent's minimal PATH — a bare `gh` spawn ENOENTs
// and the PR badge silently never resolves. Augment PATH. (CLAUDE.md L11
// LaunchAgent-env class; see listening-ports.ts for the same fix.)
const GH_PATH = `/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:${process.env.PATH ?? ""}`;

export interface PrStatus {
  number: number;
  state: "open" | "merged" | "closed" | "draft";
  checks: "passing" | "failing" | "pending" | "none";
  url: string;
}

// gh statusCheckRollup entries are heterogeneous (check runs vs legacy statuses).
// Check runs carry `.status`+`.conclusion`; commit statuses carry `.state`.
export interface RollupEntry {
  status?: string;      // QUEUED | IN_PROGRESS | COMPLETED (check runs)
  conclusion?: string;  // SUCCESS | FAILURE | ... (check runs, once COMPLETED)
  state?: string;       // SUCCESS | PENDING | FAILURE | ERROR (commit statuses)
}

export function rollupChecks(rollup: RollupEntry[] | undefined): PrStatus["checks"] {
  if (!rollup || rollup.length === 0) return "none";
  let anyPending = false;
  for (const c of rollup) {
    const concl = (c.conclusion ?? c.state ?? "").toUpperCase();
    const stat = (c.status ?? "").toUpperCase();
    if (concl === "FAILURE" || concl === "ERROR" || concl === "TIMED_OUT" || concl === "CANCELLED") return "failing";
    if (stat === "QUEUED" || stat === "IN_PROGRESS" || concl === "PENDING" || (c.state ?? "").toUpperCase() === "PENDING") anyPending = true;
    if (concl === "" && stat !== "COMPLETED" && c.state === undefined) anyPending = true; // not done yet
  }
  return anyPending ? "pending" : "passing";
}

/** PR status for `branch` in the repo at `cwd`, or null (no PR / gh unavailable).
 *  `gh` auto-detects the repo from the cwd's origin remote. */
export async function readGitPr(cwd: string, branch: string): Promise<PrStatus | null> {
  try {
    const proc = Bun.spawn(
      ["gh", "pr", "list", "--head", branch, "--state", "all", "--limit", "1",
       "--json", "number,state,isDraft,url,statusCheckRollup"],
      { cwd, stdout: "pipe", stderr: "ignore", env: { ...process.env, PATH: GH_PATH } },
    );
    const out = (await new Response(proc.stdout).text()).trim();
    await proc.exited;
    if (proc.exitCode !== 0 || out.length === 0) return null;
    const rows = JSON.parse(out) as Array<{
      number: number; state: string; isDraft: boolean; url: string;
      statusCheckRollup?: RollupEntry[];
    }>;
    const pr = rows[0];
    if (!pr) return null;
    const raw = (pr.state ?? "").toUpperCase();
    const state: PrStatus["state"] =
      raw === "MERGED" ? "merged"
      : raw === "CLOSED" ? "closed"
      : pr.isDraft ? "draft"
      : "open";
    return { number: pr.number, state, checks: rollupChecks(pr.statusCheckRollup), url: pr.url };
  } catch {
    return null; // gh missing / not authed / bad json → no badge
  }
}

/** Structural equality so the poll only emits a `pr` event on real change. */
export function prStatusEq(a: PrStatus | null | undefined, b: PrStatus | null | undefined): boolean {
  if (!a || !b) return !a && !b;
  return a.number === b.number && a.state === b.state && a.checks === b.checks && a.url === b.url;
}
