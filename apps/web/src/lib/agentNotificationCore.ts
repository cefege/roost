// Identity-pinned coding-agent notification classification and scheduling.
// Store transitions feed this owner; delayed deliveries recheck the exact
// epoch, occupant, and revision before producing browser effects.

import { isIdentifiedAgentStatus, type AgentStatus } from "@roost/shared/wire";
import type { AgentStatusChange } from "../store/agent-status.ts";
import {
  agentStatusRevisionToken,
  sameAgentStatusOccupant,
  type AgentStatusRevisionToken,
} from "./agentStatus.ts";

export type AgentNotificationKind = "blocked" | "done";

export interface AgentNotificationDelivery {
  sessionId: AgentStatus["session_id"];
  /** Original attention revision used for seen-state and cross-tab claims. */
  token: AgentStatusRevisionToken;
  /** Latest status revision that must still be current when delivery starts. */
  statusRevision: number;
  kind: AgentNotificationKind;
  completedRevision?: number;
}

export function classifyAgentTransition(
  previous: AgentStatus | null,
  next: AgentStatus,
): AgentNotificationKind | null {
  if (
    !previous
    || previous.session_id !== next.session_id
    || previous.agent_id !== next.agent_id
    || !sameAgentStatusOccupant(previous, next)
  ) return null;
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
  delivery: Pick<
    AgentNotificationDelivery,
    "token" | "statusRevision" | "kind" | "completedRevision"
  >,
): status is AgentStatus {
  if (
    !status
    || status.session_id !== delivery.token.session_id
    || status.revision !== delivery.statusRevision
    || !sameAgentStatusOccupant(status, delivery.token)
  ) return false;
  if (delivery.kind === "blocked") return status.state === "blocked";
  return status.state === "idle"
    && delivery.completedRevision !== undefined
    && status.completed_revision === delivery.completedRevision;
}

export function countUnseenAgentStatuses(
  statuses: Iterable<AgentStatus>,
  seenRevision: (status: AgentStatus) => number,
): number {
  let count = 0;
  for (const status of statuses) {
    const seen = seenRevision(status);
    if (status.state === "blocked" && status.revision > seen) count++;
    else if (
      status.state === "idle"
      && status.completed_revision > 0
      && status.completed_revision > seen
    ) count++;
  }
  return count;
}

export interface AgentNotificationSchedulerOptions {
  statusFor: (sessionId: string) => AgentStatus | undefined;
  isViewed: (sessionId: string) => boolean;
  markSeen: (status: AgentStatus) => void;
  deliver: (delivery: AgentNotificationDelivery) => void;
  delayMs?: number;
  setTimer?: (callback: () => void, delayMs: number) => ReturnType<typeof setTimeout>;
  clearTimer?: (timer: ReturnType<typeof setTimeout>) => void;
}

interface PendingAgentNotification {
  delivery: AgentNotificationDelivery;
  timer: ReturnType<typeof setTimeout>;
}

/** Ordered transition scheduler with one cancellable timer per session. */
export class AgentNotificationScheduler {
  private readonly pending = new Map<string, PendingAgentNotification>();
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
    const pending = this.pending.get(change.sessionId);
    if (!change.next) {
      this.cancel(change.sessionId);
      return;
    }
    if (this.options.isViewed(change.sessionId)) {
      this.cancel(change.sessionId);
      this.options.markSeen(change.next);
      return;
    }
    if (
      pending
      && canCarryPendingAgentNotification(pending.delivery, change.previous, change.next)
    ) {
      pending.delivery.statusRevision = change.next.revision;
      return;
    }
    this.cancel(change.sessionId);
    const kind = classifyAgentTransition(change.previous, change.next);
    if (!kind) return;
    const completedRevision = kind === "done"
      ? change.next.completed_revision
      : undefined;
    const token = agentStatusRevisionToken(change.next);
    if (completedRevision !== undefined) token.revision = completedRevision;
    const delivery: AgentNotificationDelivery = {
      sessionId: change.next.session_id,
      token,
      statusRevision: change.next.revision,
      kind,
      ...(completedRevision === undefined ? {} : { completedRevision }),
    };
    const timer = this.setTimer(() => {
      const currentPending = this.pending.get(change.sessionId);
      if (!currentPending || currentPending.delivery !== delivery) return;
      this.pending.delete(change.sessionId);
      const status = this.options.statusFor(change.sessionId);
      if (!matchesAgentNotification(status, delivery)) return;
      if (this.options.isViewed(change.sessionId)) {
        this.options.markSeen(status);
        return;
      }
      this.options.deliver(delivery);
    }, this.delayMs);
    this.pending.set(change.sessionId, { delivery, timer });
  }

  view(status: AgentStatus): void {
    this.cancel(status.session_id);
    this.options.markSeen(status);
  }

  cancel(sessionId: string): AgentNotificationDelivery | undefined {
    const pending = this.pending.get(sessionId);
    if (!pending) return undefined;
    this.clearTimer(pending.timer);
    this.pending.delete(sessionId);
    return pending.delivery;
  }

  pendingCount(): number {
    return this.pending.size;
  }

  dispose(): void {
    for (const pending of this.pending.values()) this.clearTimer(pending.timer);
    this.pending.clear();
  }
}

function canCarryPendingAgentNotification(
  delivery: AgentNotificationDelivery,
  previous: AgentStatus | null,
  next: AgentStatus,
): boolean {
  if (
    !previous
    || !isIdentifiedAgentStatus(delivery.token)
    || !isIdentifiedAgentStatus(previous)
    || !isIdentifiedAgentStatus(next)
    || delivery.statusRevision !== previous.revision
    || previous.session_id !== next.session_id
    || previous.agent_id !== next.agent_id
    || previous.state !== next.state
    || previous.completed_revision !== next.completed_revision
    || !previous.active
    || !next.active
    || !sameAgentStatusOccupant(delivery.token, previous)
    || !sameAgentStatusOccupant(previous, next)
  ) return false;
  if (delivery.kind === "blocked") return next.state === "blocked";
  return next.state === "idle"
    && delivery.completedRevision !== undefined
    && next.completed_revision === delivery.completedRevision;
}
