import type { AgentStatus } from "@roost/shared/wire";
import type { AgentStatusChange } from "../store/agent-status.ts";

export type AgentNotificationKind = "blocked" | "done";

export interface AgentNotificationDelivery {
  sessionId: string;
  revision: number;
  kind: AgentNotificationKind;
}

export function classifyAgentTransition(
  previous: AgentStatus | null,
  next: AgentStatus,
): AgentNotificationKind | null {
  if (!previous || previous.agent_id !== next.agent_id) return null;
  if (previous.state === "working" && next.state === "blocked") return "blocked";
  if (
    (previous.state === "working" || previous.state === "blocked")
    && next.state === "idle"
    && next.completed_revision === next.revision
    && next.completed_revision > previous.completed_revision
  ) return "done";
  return null;
}

export function matchesAgentNotification(
  status: AgentStatus | undefined,
  delivery: Pick<AgentNotificationDelivery, "revision" | "kind">,
): status is AgentStatus {
  if (!status || status.revision !== delivery.revision) return false;
  return delivery.kind === "blocked"
    ? status.state === "blocked"
    : status.state === "idle" && status.completed_revision === delivery.revision;
}

export function countUnseenAgentStatuses(
  statuses: Iterable<AgentStatus>,
  seenRevision: (sessionId: string) => number,
): number {
  let count = 0;
  for (const status of statuses) {
    const seen = seenRevision(status.session_id);
    if (status.state === "blocked" && status.revision > seen) count++;
    else if (status.state === "idle" && status.completed_revision > seen) count++;
  }
  return count;
}

export interface AgentNotificationSchedulerOptions {
  statusFor: (sessionId: string) => AgentStatus | undefined;
  isViewed: (sessionId: string) => boolean;
  markSeen: (sessionId: string, revision: number) => void;
  deliver: (delivery: AgentNotificationDelivery) => void;
  delayMs?: number;
  setTimer?: (callback: () => void, delayMs: number) => ReturnType<typeof setTimeout>;
  clearTimer?: (timer: ReturnType<typeof setTimeout>) => void;
}

/** Ordered transition scheduler with one cancellable timer per session. */
export class AgentNotificationScheduler {
  private readonly pending = new Map<string, {
    revision: number;
    kind: AgentNotificationKind;
    timer: ReturnType<typeof setTimeout>;
  }>();
  private readonly delayMs: number;
  private readonly setTimer: NonNullable<AgentNotificationSchedulerOptions["setTimer"]>;
  private readonly clearTimer: NonNullable<AgentNotificationSchedulerOptions["clearTimer"]>;

  constructor(private readonly options: AgentNotificationSchedulerOptions) {
    this.delayMs = options.delayMs ?? 1_000;
    // Wrapped, never stored bare: `this.setTimer(...)` would invoke
    // window.setTimeout with the scheduler as receiver, which throws
    // "Illegal invocation" in a browser (but not under Bun).
    this.setTimer = options.setTimer ?? ((callback, delayMs) => setTimeout(callback, delayMs));
    this.clearTimer = options.clearTimer ?? ((timer) => clearTimeout(timer));
  }

  handle(change: AgentStatusChange): void {
    this.cancel(change.sessionId);
    if (!change.next) return;
    if (this.options.isViewed(change.sessionId)) {
      this.options.markSeen(change.sessionId, change.next.revision);
      return;
    }
    const kind = classifyAgentTransition(change.previous, change.next);
    if (!kind) return;
    const revision = change.next.revision;
    const timer = this.setTimer(() => {
      const pending = this.pending.get(change.sessionId);
      if (!pending || pending.revision !== revision || pending.kind !== kind) return;
      this.pending.delete(change.sessionId);
      const status = this.options.statusFor(change.sessionId);
      if (!matchesAgentNotification(status, { revision, kind })) return;
      if (this.options.isViewed(change.sessionId)) {
        this.options.markSeen(change.sessionId, revision);
        return;
      }
      this.options.deliver({ sessionId: change.sessionId, revision, kind });
    }, this.delayMs);
    this.pending.set(change.sessionId, { revision, kind, timer });
  }

  view(sessionId: string, revision: number): void {
    this.cancel(sessionId);
    this.options.markSeen(sessionId, revision);
  }

  cancel(sessionId: string): void {
    const pending = this.pending.get(sessionId);
    if (!pending) return;
    this.clearTimer(pending.timer);
    this.pending.delete(sessionId);
  }

  pendingCount(): number {
    return this.pending.size;
  }

  dispose(): void {
    for (const pending of this.pending.values()) this.clearTimer(pending.timer);
    this.pending.clear();
  }
}
