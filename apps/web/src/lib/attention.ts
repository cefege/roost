import type { AgentStatus, LastMessage, Session } from "@roost/shared/wire";
import { rootStore } from "../store/root.ts";
import { workerOnline } from "../store/sync.ts";
import { lastSeenAt } from "./sessionSeen.ts";

export function liveStatus(session: Session): AgentStatus | undefined {
  return session.agent?.status;
}

export function latestAssistantOutput(session: Session): LastMessage | null {
  const message = session.agent?.last_message;
  return message?.role === "assistant" ? message : null;
}

export type AttentionKind = "blocked" | "offline" | "done" | null;

/** Reactive: requested approval, offline worker, then unseen idle output. */
export function attentionKind(s: Session): AttentionKind {
  const status = liveStatus(s);
  if (status === "needs-input") return "blocked";
  const worker = rootStore.workers[s.worker_fp];
  if (worker && !workerOnline(worker)) return "offline";
  const outputAt = latestAssistantOutput(s)?.ts ?? 0;
  return status === "idle" && outputAt > lastSeenAt(s.id) ? "done" : null;
}

/** Reactive: does this session want the user's attention? */
export function needsAttention(s: Session): boolean {
  return attentionKind(s) !== null;
}

/** The latest OMP output, falling back to session creation for stable ordering. */
function activityTs(s: Session): number {
  return latestAssistantOutput(s)?.ts || s.created_at;
}

/** The most-recently-active session needing attention, or null. */
export function mostRecentUnread(sessions: Session[]): Session | null {
  let best: Session | null = null;
  for (const s of sessions) {
    if (!needsAttention(s)) continue;
    if (!best || activityTs(s) > activityTs(best)) best = s;
  }
  return best;
}
