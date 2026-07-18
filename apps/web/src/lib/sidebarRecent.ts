// localStorage-backed recent-session stack. Last N session_ids the
// user navigated to via the sidebar, MRU first. Replaces "tab strip"
// intuition: instead of horizontal tabs, the sidebar's RECENT group
// at the bottom is the fast-switch surface.
//
// Pushed on every SessionRow click (the route enter point for /s/<id>).
// Deduped so re-clicking a recent doesn't shuffle. Capped at MAX so the
// list stays glanceable. Dead session IDs are pruned by the consumer
// (RecentGroup) at render time so we don't have to subscribe to the
// session lifecycle here.

import { createSignal } from "solid-js";

const KEY = "roost.sidebar.recent";
const MAX = 5;

function readPersisted(): string[] {
  try {
    const raw = localStorage.getItem(KEY);
    if (!raw) return [];
    const arr = JSON.parse(raw);
    return Array.isArray(arr) ? arr.filter((x): x is string => typeof x === "string") : [];
  } catch { return []; }
}

function writePersisted(next: string[]): void {
  try { localStorage.setItem(KEY, JSON.stringify(next)); } catch { /* quota / privacy mode */ }
}

const [_recent, _setRecent] = createSignal<string[]>(readPersisted(), { equals: false });

export function pushRecent(sessionId: string): void {
  const cur = _recent().filter((id) => id !== sessionId);
  cur.unshift(sessionId);
  if (cur.length > MAX) cur.length = MAX;
  _setRecent(cur);
  writePersisted(cur);
}
