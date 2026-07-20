// The one definition of "this session needs you", shared by the sidebar's
// attention band AND the ⌘⇧U jump-to-most-recent-unread. The
// multi-signal model, Roost flavor: agent blocked on input, OR its worker went
// offline (connection-trouble), OR a finished agent (idle/done) produced output
// newer than when you last looked (unread). See lib/sessionSeen.ts for "seen".

import type { Session } from "@roost/shared/wire";
import { rootStore } from "../store/root.ts";
import { workerOnline } from "../store/sync.ts";
import { lastSeenAt } from "./sessionSeen.ts";

/** Resolved live status: server agent.status, falling back to grid-scraped. */
export function liveStatus(s: Session): string | undefined {
  return s.agent?.status ?? rootStore.claude_status[s.id];
}

export type AttentionKind = "blocked" | "offline" | "done" | null;

/** Reactive (reads store + seen-map): WHY this session wants your attention,
 *  or null. Precedence: needs-input → offline worker → idle/done with unseen
 *  output. One source of truth for the sidebar sort and the folder badge. */
export function attentionKind(s: Session): AttentionKind {
  const st = liveStatus(s);
  if (st === "needs-input") return "blocked";
  const w = rootStore.workers[s.worker_fp];
  if (w && !workerOnline(w)) return "offline"; // stranded on an offline/asleep Mac
  if (st === "idle" || st === "done") {
    const lm = s.agent?.last_message?.ts ?? 0;
    if (lm > 0 && lm > lastSeenAt(s.id)) return "done"; // finished with unseen output
  }
  return null;
}

/** Reactive (reads store + seen-map): does this session want your attention? */
export function needsAttention(s: Session): boolean {
  return attentionKind(s) !== null;
}

function activityTs(s: Session): number {
  return s.agent?.last_message?.ts ?? s.created_at;
}

/** The most-recently-active session needing attention, or null (⌘⇧U target). */
export function mostRecentUnread(sessions: Session[]): Session | null {
  let best: Session | null = null;
  for (const s of sessions) {
    if (!needsAttention(s)) continue;
    if (!best || activityTs(s) > activityTs(best)) best = s;
  }
  return best;
}
