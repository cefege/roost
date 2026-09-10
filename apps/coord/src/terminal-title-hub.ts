// Coordinator-owned retained terminal titles.
// The worker transport adapter supplies validated semantic title observations;
// this hub deduplicates spinner animation and fans the latest value to Sync.
// Session close events release retained state.

import { diag } from "@roost/shared/diag";
import { normalizeTerminalTitle } from "@roost/shared/terminal-metadata";
import { sessionBus, titleBus } from "./buses.ts";


interface Entry {
  last: string | null;
  lastKey: string | null;
}

const _entries = new Map<string, Entry>();

/** Accept one semantic title observation from a worker transport route. */
export function observeTerminalTitle(sessionId: string, rawTitle: string): void {
  const { title, dedupKey } = normalizeTerminalTitle(rawTitle);
  let entry = _entries.get(sessionId);
  if (!entry) {
    entry = { last: null, lastKey: null };
    _entries.set(sessionId, entry);
  }
  if (entry.lastKey === dedupKey) return;
  entry.last = title;
  entry.lastKey = dedupKey;
  titleBus.publish({ session_id: sessionId, title });
  diag("terminal_title.change", { sid: sessionId });
}

/** Current title per session — replayed to each new Sync subscriber so a fresh
 *  page load reflects the live title immediately (titleBus is publish-on-change,
 *  not backfilled). */
export function getTitleSnapshot(): Array<{ session_id: string; title: string }> {
  const out: Array<{ session_id: string; title: string }> = [];
  for (const [sid, e] of _entries) {
    if (e.last !== null) out.push({ session_id: sid, title: e.last });
  }
  return out;
}

export function startTerminalTitleHub(): () => void {
  const unsubSessions = sessionBus.subscribe((event) => {
    if (event.kind === "closed") _entries.delete(event.session_id);
  });
  return () => {
    unsubSessions();
    _entries.clear();
  };
}
