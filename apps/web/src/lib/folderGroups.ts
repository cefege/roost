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
import { liveStatus, attentionKind } from "./attention.ts";
import { activityLine, type Attention } from "./folderSubtitle.ts";
import { shortServerLabel } from "./sidebarFormat.ts";
import { workerOnline } from "../store/sync.ts";
import { folderKeyOf, folderPathOf, folderDisplayName } from "./folderKey.ts";
import { isClaudeSession } from "./isClaudeSession.ts";
import { unreadByFolder } from "./notifyStore.ts";

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
  attention: Attention;   // coarse band (needs → running → idle), derived from priority
  priority: number;       // fine sort key: blocked=3 > offline|done=2 > running=1 > idle=0
  online: boolean;        // worker reachable → server-icon online dot
  isClaude: boolean;      // leading glyph: claude mark vs terminal $
  glyphStatus: GlyphStatus; // leading glyph status color/icon
  subtitle: string;       // latest activity line ("" = none, row shows one line)
  latestActivity: number;  // sort key + row stamp: newest real activity
                           // (max last-PTY-byte across panes, fallback
                           // agent last-message ts, fallback created_at)
  leadId: string;         // click target (neediest / most-recent)
  sessionIds: string[];   // ids only — plain scalars so reconcile diffs values,
                          // never deep-walks live Session store proxies
  pr: PrBadge | null;     // github PR status of the lead's branch (worker gh)
  branch: string | null;  // current git branch of the lead's cwd (worker git)
  ports: number[];        // LISTEN ports across the folder's panes (worker lsof)
  reachAddr: string | null; // worker host for port click-through (reachable_addr)
  unreadCount: number;    // unread attention-notifications for this folder (bell badge)
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

// Lead's last-message ts, falling back to session creation. Fallback inside
// recencyOf for sessions with no coord last-PTY-byte stamp yet.
function activityTsOf(s: Session): number {
  return s.agent?.last_message?.ts ?? s.created_at;
}

// Most-recent real activity for a session: coord's last-PTY-byte stamp
// (last_activity — bytes from terminals AND agents) when present, else the
// agent's last-message ts, else created_at. last_activity is absent for
// sessions idle since before coord start / this page load, so the fallback
// keeps them ordered sanely.
function recencyOf(s: Session): number {
  return Math.max(rootStore.last_activity[s.id] ?? 0, activityTsOf(s));
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

// Fine-grained folder sort rank from attentionKind across its panes:
// blocked=3 (waiting on YOU) > offline|done=2 (rest of the needs band) >
// running=1 > idle=0. The coarse `attention` band is derived from this.
function folderPriority(sessions: Session[]): number {
  let p = 0; // 0 idle/calm
  for (const s of sessions) {
    const k = attentionKind(s);
    if (k === "blocked") return 3; // top — early out
    if (k === "offline" || k === "done") p = 2;
  }
  if (p > 0) return p;
  return sessions.some((s) => {
    const st = liveStatus(s);
    return st === "running" || st === "running-workflow";
  }) ? 1 : 0;
}

// `input` defaults to the reactive allSessions() (default evaluated at call
// time, so prod callers stay fully reactive); tests pass a fixed array to sort.
export function buildFolderGroups(input: Session[] = allSessions()): FolderGroup[] {
  const list = input.filter((s) => !isPendingClose(s.id));
  const buckets = new Map<string, Session[]>();
  for (const s of list) {
    const key = folderKeyOf(s);
    (buckets.get(key) ?? buckets.set(key, []).get(key)!).push(s);
  }
  const out: FolderGroup[] = [];
  for (const [key, sessions] of buckets) {
    const pr = folderPriority(sessions);
    const attention: Attention = pr >= 2 ? "needs" : pr === 1 ? "running" : "idle";
    // lead = neediest-then-most-recent (for needs), else most-recent activity.
    const pool = pr >= 2 ? sessions.filter((s) => attentionKind(s) !== null) : sessions;
    const lead = [...pool].sort((a, b) => recencyOf(b) - recencyOf(a))[0];
    const head = sessions[0];
    const gs = glyphStatusOf(lead);
    const latestActivity = Math.max(...sessions.map(recencyOf));
    out.push({
      key,
      name: folderDisplayName(head),
      server: shortServerLabel(rootStore.workers[head.worker_fp]?.label ?? String(head.worker_fp).slice(0, 6)),
      spawnFp: head.worker_fp,
      spawnCwd: folderPathOf(head),
      attention,
      priority: pr,
      online: workerOnline(rootStore.workers[head.worker_fp]),
      isClaude: isClaudeSession(lead),
      glyphStatus: gs,
      subtitle: activityLine(lead, attention),
      latestActivity,
      pr: prBadgeOf(lead),
      branch: lead.git_branch ?? null,
      // Union of LISTEN ports across the folder's panes, ascending.
      ports: [...new Set(sessions.flatMap((s) => s.ports ?? []))].sort((a, b) => a - b),
      reachAddr: rootStore.workers[head.worker_fp]?.reachable_addr ?? null,
      leadId: lead.id,
      sessionIds: sessions.map((s) => s.id),
      unreadCount: unreadByFolder()[key] ?? 0,
    });
  }
  // Blocked-on-you first, then offline/done, then running, then most-recent.
  return out.sort((a, b) => b.priority - a.priority || b.latestActivity - a.latestActivity);
}
