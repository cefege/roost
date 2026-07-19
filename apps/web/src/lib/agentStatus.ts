// The ONE agent-attention vocab — shared by the sidebar sort, the pane/tab dots,
// and any status surface. Rolls a session's raw status (running / needs-input /
// idle / … via liveStatus, plus the seen-map via needsAttention) up to herdr's
// 5-level attention model: blocked > done > working > idle > unknown. "done" =
// a finished agent whose output you haven't seen (idle/done & unseen — falls out
// of needsAttention's idle branch, attention.ts:23). Replaces 3 duplicated
// status→color maps (PaneStrip / SessionRow / StatusGlyph);
// uses --md-* design-system roles only.

import type { Session } from "@roost/shared/wire";
import { rootStore } from "../store/root.ts";
import { workerOnline } from "../store/sync.ts";
import { liveStatus, needsAttention } from "./attention.ts";

export type AttentionLevel = "blocked" | "done" | "working" | "idle" | "unknown";

const RANK: Record<AttentionLevel, number> = { blocked: 4, done: 3, working: 2, idle: 1, unknown: 0 };
export function rankOf(level: AttentionLevel): number { return RANK[level]; }

/** blocked / done / working want a dot; idle / unknown are calm (no dot). */
export function isActionable(level: AttentionLevel): boolean {
  return rankOf(level) >= rankOf("working");
}

/** Reactive: roll ONE session up to its attention level (reads store + seen-map). */
export function attentionOf(s: Session): AttentionLevel {
  if (needsAttention(s)) {
    // needsAttention = needs-input | offline worker | (idle/done & unseen output)
    const st = liveStatus(s);
    const w = rootStore.workers[s.worker_fp];
    const offline = !!w && !workerOnline(w);
    return st === "needs-input" || offline ? "blocked" : "done";
  }
  const st = liveStatus(s);
  if (st === "running" || st === "running-workflow") return "working";
  if (st === "idle" || st === "done") return "idle"; // finished + seen → calm
  return "unknown";
}

/** Pure: fold a set of levels to the highest (herdr group = max). Testable. */
export function rollupLevels(levels: AttentionLevel[]): AttentionLevel {
  let best: AttentionLevel = "unknown";
  for (const l of levels) if (rankOf(l) > rankOf(best)) best = l;
  return best;
}

export interface StatusVisual {
  color: string;
  label: string;
  short: string;
}

// One status→visual map, --md-* roles only. Every level is a distinct color:
// blocked=amber (--md-warning), working=accent, done=secondary, idle=success,
// unknown=dim. `short` = the 1–2 word pill text; `label` = the long descriptive
// tooltip/aria text.
const VISUAL: Record<AttentionLevel, StatusVisual> = {
  blocked: { color: "var(--md-warning)", label: "Needs input — waiting on you", short: "Needs input" },
  done: { color: "var(--md-secondary)", label: "Done — finished while you were away", short: "Done" },
  working: { color: "var(--md-primary)", label: "Working", short: "Working" },
  idle: { color: "var(--md-success)", label: "Idle — finished, seen", short: "Idle" },
  unknown: { color: "var(--md-on-surface-dim)", label: "Idle", short: "Idle" },
};

/** The one status→{color,label} map (replaces the 4 duplicated ones). */
export function presentationOf(level: AttentionLevel): StatusVisual {
  return VISUAL[level];
}
