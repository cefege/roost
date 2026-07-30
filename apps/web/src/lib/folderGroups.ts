// Shared folder-grouping model — the (worker, folder) bucketing the sidebar
// (FolderList) and the home folder grid (HomeLanding) both render. Extracted
// from FolderList.tsx so the two surfaces group sessions identically.
//
// Pure read of rootStore + allSessions(); no setStore. Sort: terminal recency.

import type { Session, WorkerFp } from "@roost/shared/wire";
import { rootStore } from "../store/root.ts";
import { allSessions } from "../store/selectors.ts";
import { isPendingClose } from "./pendingClose.ts";
import { shortServerLabel } from "./sidebarFormat.ts";
import { workerOnline } from "../store/sync.ts";
import { folderKeyOf, folderPathOf, folderDisplayName } from "./folderKey.ts";


// Lead-session PR status, shaped for the row badge. null = no PR to show.
export interface PrBadge {
  number: number;
  state: "open" | "merged" | "closed" | "draft";
  checks: "passing" | "failing" | "pending" | "none";
  url: string;
}

export interface FolderGroup {
  key: string;
  name: string;
  server: string;
  spawnFp: WorkerFp;
  spawnCwd: string;
  online: boolean;        // worker reachable → server-icon online dot
  subtitle: string;       // worker-offline explanation ("" when reachable)
  latestActivity: number; // max last terminal byte, falling back to creation
  leadId: string;         // most-recent terminal; click target
  sessionIds: string[];   // ids only — plain scalars so reconcile diffs values,
                          // never deep-walks live Session store proxies
  pr: PrBadge | null;     // github PR status of the lead's branch (worker gh)
  branch: string | null;  // current git branch of the lead's cwd (worker git)
  ports: number[];        // LISTEN ports across the folder's panes (worker lsof)
  reachAddr: string | null; // worker host for port click-through (reachable_addr)
}

// Check glyph + token color per rollup state. none → no glyph (just #123).
export const PR_CHECK_GLYPH: Record<PrBadge["checks"], string> = {
  passing: "✓", failing: "✕", pending: "•", none: "",
};
export const PR_CHECK_COLOR: Record<PrBadge["checks"], string> = {
  passing: "var(--color-ok)", failing: "var(--color-warn)",
  pending: "var(--text-lo)", none: "var(--text-lo)",
};

// Most-recent terminal activity, falling back to creation when the coord has
// not observed a PTY byte for the session yet.
function recencyOf(s: Session): number {
  return Math.max(rootStore.last_activity[s.id] ?? 0, s.created_at);
}

// Pull the lead session's PR fields into a badge, or null when absent.
function prBadgeOf(lead: Session): PrBadge | null {
  if (lead.pr_number == null) return null;
  return {
    number: lead.pr_number,
    state: (lead.pr_state ?? "open") as PrBadge["state"],
    checks: (lead.pr_checks ?? "none") as PrBadge["checks"],
    url: lead.pr_url ?? "",
  };
}


// `input` defaults to the reactive allSessions() (default evaluated at call
// time, so prod callers stay fully reactive); tests pass a fixed array to sort.
export function buildFolderGroups(input: Session[] = allSessions()): FolderGroup[] {
  const list = input.filter((s) => s.kind === "shell" && !isPendingClose(s.id));
  const buckets = new Map<string, Session[]>();
  for (const s of list) {
    const key = folderKeyOf(s);
    (buckets.get(key) ?? buckets.set(key, []).get(key)!).push(s);
  }
  const out: FolderGroup[] = [];
  for (const [key, sessions] of buckets) {
    const lead = [...sessions].sort((a, b) => recencyOf(b) - recencyOf(a))[0];
    const head = sessions[0];
    const worker = rootStore.workers[head.worker_fp];
    const latestActivity = Math.max(...sessions.map(recencyOf));
    const online = worker ? workerOnline(worker) : false;
    out.push({
      key,
      name: folderDisplayName(head),
      server: shortServerLabel(worker?.label ?? String(head.worker_fp).slice(0, 6)),
      spawnFp: head.worker_fp,
      spawnCwd: folderPathOf(head),
      online,
      subtitle: online ? "" : "Machine offline — reopen to refresh",
      latestActivity,
      pr: prBadgeOf(lead),
      branch: lead.git_branch ?? null,
      // Union of LISTEN ports across the folder's panes, ascending.
      ports: [...new Set(sessions.flatMap((s) => s.ports ?? []))].sort((a, b) => a - b),
      reachAddr: worker?.reachable_addr ?? null,
      leadId: lead.id,
      sessionIds: sessions.map((s) => s.id),
    });
  }
  return out.sort((a, b) => b.latestActivity - a.latestActivity);
}
