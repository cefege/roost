// Volatile coding-agent status projection. Sync frames are admitted by exact
// worker epoch and process occupant; displaced identities remain retired so
// delayed frames cannot reclaim a session or erase its current occupant.

import {
  AgentStatus,
  AgentStatusUpdate,
  isIdentifiedAgentStatus,
  type AgentOccupantId,
  type AgentStatus as AgentStatusValue,
  type AgentStatusIdentity,
  type AgentStatusUpdate as AgentStatusUpdateValue,
  type StatusEpoch,
} from "@roost/shared/wire";
import type { AgentStatusFrame } from "@roost/shared/proto/sync_pb";
import { signal } from "@roost/shared/diag";
import { sameAgentStatusOccupant } from "../lib/agentStatus.ts";
import { deleteStoreRecord, rootStore, setRootStore } from "./root.ts";

export interface AgentStatusChange {
  sessionId: string;
  previous: AgentStatusValue | null;
  next: AgentStatusValue | null;
  revision: number;
}

interface AgentStatusAdmission {
  legacyFloor: number;
  identifiedSeen: boolean;
  retiredEpochs: Set<StatusEpoch>;
  retiredOccupantsByEpoch: Map<StatusEpoch, Set<AgentOccupantId>>;
  latestStatusEpoch?: StatusEpoch;
  latestOccupantId?: AgentOccupantId;
}

const admissionBySession = new Map<string, AgentStatusAdmission>();
const subscribers = new Set<(change: AgentStatusChange) => void>();
const closedSessionIds = new Set<string>();

function publish(change: AgentStatusChange): void {
  for (const subscriber of subscribers) {
    try { subscriber(change); }
    catch (error) {
      signal("diag.corruption_signal", {
        kind: "agent_status_subscriber_failed",
        sid: change.sessionId,
        msg: String(error),
        cooldownKey: change.sessionId,
      });
    }
  }
}

export function subscribeAgentStatus(
  subscriber: (change: AgentStatusChange) => void,
): () => void {
  subscribers.add(subscriber);
  return () => subscribers.delete(subscriber);
}

/** An authoritative session upsert starts a fresh browser lifecycle fence. */
export function markAgentStatusSessionOpen(sessionId: string): void {
  closedSessionIds.delete(sessionId);
}

/** Store reads return live proxies: a subscriber would otherwise see the
 *  POST-update value as `previous` and every transition would look like a
 *  self-transition. Every field is primitive, so a shallow copy detaches it. */
function detach(current: AgentStatusValue | undefined): AgentStatusValue | null {
  return current ? { ...current } : null;
}

function admissionFor(
  sessionId: string,
  current: AgentStatusValue | null,
): AgentStatusAdmission {
  const existing = admissionBySession.get(sessionId);
  if (existing) return existing;
  const identifiedCurrent = current && isIdentifiedAgentStatus(current)
    ? current
    : null;
  const admission: AgentStatusAdmission = {
    legacyFloor: current && !identifiedCurrent ? current.revision : -1,
    identifiedSeen: identifiedCurrent !== null,
    retiredEpochs: new Set(),
    retiredOccupantsByEpoch: new Map(),
    latestStatusEpoch: identifiedCurrent?.status_epoch,
    latestOccupantId: identifiedCurrent?.occupant_id,
  };
  admissionBySession.set(sessionId, admission);
  return admission;
}

function isRetiredIdentity(
  admission: AgentStatusAdmission,
  identity: AgentStatusIdentity,
): boolean {
  return admission.retiredEpochs.has(identity.status_epoch)
    || admission.retiredOccupantsByEpoch
      .get(identity.status_epoch)
      ?.has(identity.occupant_id) === true;
}

function retireOccupant(
  admission: AgentStatusAdmission,
  statusEpoch: StatusEpoch,
  occupantId: AgentOccupantId,
): void {
  let retired = admission.retiredOccupantsByEpoch.get(statusEpoch);
  if (!retired) {
    retired = new Set();
    admission.retiredOccupantsByEpoch.set(statusEpoch, retired);
  }
  retired.add(occupantId);
}

function retireEpoch(
  admission: AgentStatusAdmission,
  statusEpoch: StatusEpoch,
): void {
  admission.retiredEpochs.add(statusEpoch);
  admission.retiredOccupantsByEpoch.delete(statusEpoch);
}

function advanceIdentity(
  admission: AgentStatusAdmission,
  identity: AgentStatusIdentity,
): void {
  if (admission.latestStatusEpoch !== undefined) {
    if (admission.latestStatusEpoch !== identity.status_epoch) {
      retireEpoch(admission, admission.latestStatusEpoch);
    } else if (
      admission.latestOccupantId !== undefined
      && admission.latestOccupantId !== identity.occupant_id
    ) {
      retireOccupant(
        admission,
        admission.latestStatusEpoch,
        admission.latestOccupantId,
      );
    }
  }
  admission.latestStatusEpoch = identity.status_epoch;
  admission.latestOccupantId = identity.occupant_id;
}

function acceptsStatus(
  admission: AgentStatusAdmission,
  previous: AgentStatusValue | null,
  update: AgentStatusUpdateValue,
): boolean {
  if (!isIdentifiedAgentStatus(update)) {
    if (admission.identifiedSeen || (previous && isIdentifiedAgentStatus(previous))) return false;
    if (update.revision <= admission.legacyFloor) return false;
    return update.active || previous !== null;
  }
  if (isRetiredIdentity(admission, update)) return false;
  if (!previous) return update.active;
  if (!isIdentifiedAgentStatus(previous)) return update.active;
  if (!sameAgentStatusOccupant(previous, update)) return update.active;
  return update.revision > previous.revision;
}

function recordAcceptedStatus(
  admission: AgentStatusAdmission,
  update: AgentStatusUpdateValue,
): void {
  if (!isIdentifiedAgentStatus(update)) {
    admission.legacyFloor = update.revision;
    return;
  }
  admission.identifiedSeen = true;
  advanceIdentity(admission, update);
  if (!update.active) {
    retireOccupant(admission, update.status_epoch, update.occupant_id);
  }
}

/** Validate, fence, and project one Sync AgentStatusFrame. */
export function applyAgentStatusFrame(frame: AgentStatusFrame): boolean {
  const parsed = AgentStatusUpdate.safeParse({
    session_id: frame.sessionId,
    agent_id: frame.agentId,
    state: frame.state,
    message: frame.message,
    revision: Number(frame.revision),
    completed_revision: Number(frame.completedRevision),
    updated_at: frame.updatedAt,
    active: frame.active,
    status_epoch: frame.statusEpoch,
    occupant_id: frame.occupantId,
    source: frame.source,
  });
  if (!parsed.success) {
    signal("diag.corruption_signal", {
      kind: "invalid_agent_status",
      sid: frame.sessionId,
      cooldownKey: frame.sessionId,
    });
    return false;
  }
  if (closedSessionIds.has(frame.sessionId)) return false;

  const update = parsed.data;
  const current = detach(rootStore.agent_status[update.session_id] as AgentStatusValue | undefined);
  const admission = admissionFor(update.session_id, current);
  if (!acceptsStatus(admission, current, update)) return false;
  recordAcceptedStatus(admission, update);

  if (update.active) {
    const active = AgentStatus.parse(update);
    setRootStore("agent_status", active.session_id, active);
    publish({
      sessionId: active.session_id,
      previous: current,
      next: active,
      revision: active.revision,
    });
  } else {
    deleteStoreRecord("agent_status", update.session_id);
    publish({
      sessionId: update.session_id,
      previous: current,
      next: null,
      revision: update.revision,
    });
  }
  return true;
}

/** Remove volatile status when the durable session projector removes a row. */
export function clearAgentStatusForSession(sessionId: string): void {
  closedSessionIds.add(sessionId);
  const current = detach(rootStore.agent_status[sessionId] as AgentStatusValue | undefined);
  if (!current) return;
  const admission = admissionFor(sessionId, current);
  if (isIdentifiedAgentStatus(current)) {
    admission.identifiedSeen = true;
    advanceIdentity(admission, current);
    retireOccupant(admission, current.status_epoch, current.occupant_id);
  } else {
    admission.legacyFloor = Math.max(admission.legacyFloor, current.revision);
  }
  deleteStoreRecord("agent_status", sessionId);
  publish({ sessionId, previous: current, next: null, revision: current.revision });
}

/** Drop every dashboard's identity fences and notify subscribers so delayed
 * notification timers cannot outlive the resources they describe. */
export function resetAgentStatusProjection(): void {
  const currentStatuses = Object.entries(rootStore.agent_status);
  admissionBySession.clear();
  closedSessionIds.clear();
  for (const [sessionId, current] of currentStatuses) {
    deleteStoreRecord("agent_status", sessionId);
    publish({
      sessionId,
      previous: detach(current),
      next: null,
      revision: current.revision,
    });
  }
}
