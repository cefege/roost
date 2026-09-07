// Owns coordinator admission order for one session's observed agent status.
// Revisions order updates only inside an exact epoch/occupant tuple; retired
// UUID tokens are compared solely by equality and remain fenced through a
// close/open boundary until the hub itself is stopped. It also marks the
// revision at which the latest occupant's state last changed, which is the
// only advance a status wait may honour.

import {
  isIdentifiedAgentStatus,
  type AgentOccupantId,
  type AgentRuntimeState,
  type AgentStatus,
  type AgentStatusIdentity,
  type AgentStatusUpdate,
  type StatusEpoch,
} from "@roost/shared/wire";

export function sameAgentStatusOccupant(
  left: AgentStatus | AgentStatusUpdate,
  right: AgentStatus | AgentStatusUpdate,
): boolean {
  const leftIdentified = isIdentifiedAgentStatus(left);
  const rightIdentified = isIdentifiedAgentStatus(right);
  if (!leftIdentified || !rightIdentified) return leftIdentified === rightIdentified;
  return left.status_epoch === right.status_epoch
    && left.occupant_id === right.occupant_id;
}

export class AgentStatusOrder {
  private identifiedAccepted = false;
  private legacyRevision = -1;
  private readonly retiredEpochs = new Set<StatusEpoch>();
  private readonly retiredOccupantsByEpoch = new Map<StatusEpoch, Set<AgentOccupantId>>();
  private latestStatusEpoch: StatusEpoch | undefined;
  private latestOccupantId: AgentOccupantId | undefined;
  private latestState: AgentRuntimeState | undefined;
  private latestStateChangeRevision = 0;

  accepts(previous: AgentStatus | undefined, update: AgentStatusUpdate): boolean {
    if (!isIdentifiedAgentStatus(update)) {
      if (this.identifiedAccepted || (previous && isIdentifiedAgentStatus(previous))) return false;
      if (update.revision <= this.legacyRevision) return false;
      return update.active || previous !== undefined;
    }
    if (this.isRetired(update)) return false;
    if (!previous) return update.active;
    if (!isIdentifiedAgentStatus(previous)) return update.active;
    if (!sameAgentStatusOccupant(previous, update)) return update.active;
    return update.revision > previous.revision;
  }

  record(update: AgentStatusUpdate): void {
    if (!isIdentifiedAgentStatus(update)) {
      this.legacyRevision = update.revision;
      return;
    }
    // Waiters advance on a real transition, so the change point moves only when
    // this occupant's state differs from the state last observed for it.
    if (
      this.latestStatusEpoch !== update.status_epoch
      || this.latestOccupantId !== update.occupant_id
      || this.latestState !== update.state
    ) {
      this.latestStateChangeRevision = update.revision;
    }
    this.latestState = update.state;
    this.identifiedAccepted = true;
    this.advanceIdentity(update);
    if (!update.active) {
      this.retireOccupant(update.status_epoch, update.occupant_id);
    }
  }

  /** Revision at which the latest occupant's state last actually changed. */
  get stateChangeRevision(): number {
    return this.latestStateChangeRevision;
  }

  recordClose(current: AgentStatus, inactiveRevision: number): void {
    if (isIdentifiedAgentStatus(current)) {
      this.identifiedAccepted = true;
      this.advanceIdentity(current);
      this.retireOccupant(current.status_epoch, current.occupant_id);
    } else {
      this.legacyRevision = Math.max(this.legacyRevision, inactiveRevision);
    }
  }

  private isRetired(identity: AgentStatusIdentity): boolean {
    return this.retiredEpochs.has(identity.status_epoch)
      || this.retiredOccupantsByEpoch
        .get(identity.status_epoch)
        ?.has(identity.occupant_id) === true;
  }

  private advanceIdentity(identity: AgentStatusIdentity): void {
    if (this.latestStatusEpoch !== undefined) {
      if (this.latestStatusEpoch !== identity.status_epoch) {
        this.retireEpoch(this.latestStatusEpoch);
      } else if (
        this.latestOccupantId !== undefined
        && this.latestOccupantId !== identity.occupant_id
      ) {
        this.retireOccupant(this.latestStatusEpoch, this.latestOccupantId);
      }
    }
    this.latestStatusEpoch = identity.status_epoch;
    this.latestOccupantId = identity.occupant_id;
  }

  private retireOccupant(
    statusEpoch: StatusEpoch,
    occupantId: AgentOccupantId,
  ): void {
    let retired = this.retiredOccupantsByEpoch.get(statusEpoch);
    if (!retired) {
      retired = new Set();
      this.retiredOccupantsByEpoch.set(statusEpoch, retired);
    }
    retired.add(occupantId);
  }

  private retireEpoch(statusEpoch: StatusEpoch): void {
    this.retiredEpochs.add(statusEpoch);
    this.retiredOccupantsByEpoch.delete(statusEpoch);
  }
}
