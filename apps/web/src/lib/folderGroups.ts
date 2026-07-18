// Shared folder-grouping model — the (worker, folder) bucketing the sidebar
// (FolderList) and the home folder grid (HomeLanding) both render. Extracted
// from FolderList.tsx so the two surfaces group sessions identically.
//
// Pure read of rootStore + allSessions(); no setStore. Sort: needs → running
// → recency (the needs-you-first ordering that's core to Roost — a Drive
// modified-time sort would bury the folder waiting on you).

import type { Session, WorkerFp } from "@roost/shared/wire";
import { rootStore } from "../store/root.ts";
import { allSessions } from "../store/selectors.ts";
import { isPendingClose } from "./pendingClose.ts";
import { liveStatus, needsAttention } from "./attention.ts";
import { activityLine, type Attention } from "./folderSubtitle.ts";
import { shortServerLabel } from "./sidebarFormat.ts";
import { workerOnline } from "../store/sync.ts";
import { folderKeyOf, folderPathOf, folderDisplayName } from "./folderKey.ts";
import { isClaudeSession } from "./isClaudeSession.ts";

export type { Attention };

export type GlyphStatus = "running" | "needs-input" | "idle" | "done" | undefined;

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
  attention: Attention;   // sort key (needs → running → idle)
  online: boolean;        // worker reachable → server-icon online dot
  isClaude: boolean;      // leading glyph: claude mark vs terminal $
  glyphStatus: GlyphStatus; // leading glyph status color/icon
  subtitle: string;       // latest activity line ("" = none, row shows one line)
  activityTs: number;     // sort key = lead's last-message ts
  ageTs: number;          // stamp shown on the row: for a WAITING agent this is
                          // when it went idle (last PTY byte) so the label reads
                          // "how long unattended"; else falls back to activityTs
  leadId: string;         // click target (neediest / most-recent)
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

// Map the folder lead's live status into StatusGlyph's vocabulary (same
// coercion SessionRow uses: running-workflow → running; unknown → undefined).
// Names a non-obvious coercion that would read as a bug if inlined.
function glyphStatusOf(lead: Session): GlyphStatus {
  const s = liveStatus(lead);
  if (s === "running-workflow") return "running";
  if (s === "running" || s === "needs-input" || s === "idle" || s === "done") return s;
  return undefined;
}

// Lead's last-message ts, falling back to session creation. 4 call sites in
// buildFolderGroups (sort comparator ×2, activityTs, ageTs fallback) — lockstep.
function activityTsOf(s: Session): number {
  return s.agent?.last_message?.ts ?? s.created_at;
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

export function buildFolderGroups(): FolderGroup[] {
  const list = allSessions().filter((s) => !isPendingClose(s.id));
  const buckets = new Map<string, Session[]>();
  for (const s of list) {
    const key = folderKeyOf(s);
    (buckets.get(key) ?? buckets.set(key, []).get(key)!).push(s);
  }
  const out: FolderGroup[] = [];
  for (const [key, sessions] of buckets) {
    const anyNeeds = sessions.some(needsAttention);
    const anyRunning = sessions.some((s) => {
      const st = liveStatus(s);
      return st === "running" || st === "running-workflow";
    });
    const attention: Attention = anyNeeds ? "needs" : anyRunning ? "running" : "idle";
    // lead = neediest-then-most-recent (for needs), else most-recent activity.
    const pool = anyNeeds ? sessions.filter(needsAttention) : sessions;
    const lead = [...pool].sort((a, b) => activityTsOf(b) - activityTsOf(a))[0];
    const head = sessions[0];
    const gs = glyphStatusOf(lead);
    // Waiting agent (idle/done/needs-input) → stamp = time since it went idle
    // (coord's last-PTY-byte last_activity ≈ the moment claude stopped). That's
    // the "how long left unattended" number (SessionRow uses the same source).
    // cell-grid model (ws.latestAt): the clock starts when the WHOLE workspace
    // went quiet, i.e. the most-recent last-PTY-byte across its panes. You're
    // only "unattended" once everything stopped — a pane that moved 20s ago
    // means you weren't idle for the hour the oldest pane has sat. (max ts.)
    const latestQuiet =
      (gs === "idle" || gs === "done" || gs === "needs-input")
        ? Math.max(...sessions.map((s) => rootStore.last_activity[s.id] ?? 0))
        : 0;
    const waitingSince = latestQuiet > 0 ? latestQuiet : undefined;
    out.push({
      key,
      name: folderDisplayName(head),
      server: shortServerLabel(rootStore.workers[head.worker_fp]?.label ?? String(head.worker_fp).slice(0, 6)),
      spawnFp: head.worker_fp,
      spawnCwd: folderPathOf(head),
      attention,
      online: workerOnline(rootStore.workers[head.worker_fp]),
      isClaude: isClaudeSession(lead),
      glyphStatus: gs,
      subtitle: activityLine(lead, attention),
      activityTs: activityTsOf(lead),
      ageTs: waitingSince ?? activityTsOf(lead),
      pr: prBadgeOf(lead),
      branch: lead.git_branch ?? null,
      // Union of LISTEN ports across the folder's panes, ascending.
      ports: [...new Set(sessions.flatMap((s) => s.ports ?? []))].sort((a, b) => a - b),
      reachAddr: rootStore.workers[head.worker_fp]?.reachable_addr ?? null,
      leadId: lead.id,
      sessionIds: sessions.map((s) => s.id),
    });
  }
  // Needs-you first, then running, then most-recent activity.
  const rank: Record<Attention, number> = { needs: 2, running: 1, idle: 0 };
  return out.sort((a, b) => {
    if (rank[a.attention] !== rank[b.attention]) return rank[b.attention] - rank[a.attention];
    return b.activityTs - a.activityTs;
  });
}
