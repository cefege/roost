// Volatile coding-agent status projection. Sync frames are admitted by exact
// worker epoch and process occupant; displaced identities remain retired so
// delayed frames cannot reclaim a session or erase its current occupant.
// Every admitted change also takes a browser-assigned arrival number, because
// attention ordering across machines cannot compare producing workers' clocks.
// A row whose occupant already exited is kept only while this browser profile
// still owes it a completion; acknowledgement retires it (./agentSeen.ts).

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
import { seenAgentRevision } from "../lib/agentSeen.ts";
import { deriveAgentStatusLevel, sameAgentStatusOccupant } from "../lib/agentStatus.ts";
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

const arrivalBySession = new Map<string, { identity: string; arrival: number }>();
let lastArrival = 0;

/** Browser arrival order of a session's current status; 0 when it has none.
 *  Monotonic per browser, so a worker whose wall clock runs ahead cannot pin
 *  its sessions to the top of an attention list. */
export function agentStatusArrival(sessionId: string): number {
  return arrivalBySession.get(sessionId)?.arrival ?? 0;
}

function recordArrival(update: AgentStatusUpdateValue): void {
  const identity = isIdentifiedAgentStatus(update)
    ? `${update.status_epoch}:${update.occupant_id}:${update.revision}`
    : `:${update.revision}`;
  if (arrivalBySession.get(update.session_id)?.identity === identity) return;
  lastArrival += 1;
  arrivalBySession.set(update.session_id, { identity, arrival: lastArrival });
}

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

/** A released occupant's row is retained only to carry the completion that
 *  occupant earned. Presented as anything but Done it describes an agent that
 *  is gone and a completion this profile already acknowledged, so it is spent
 *  and retires exactly as an inactive frame would retire it. */
function releasedOccupantIsSpent(status: AgentStatusValue): boolean {
  return status.occupant_exited
    && deriveAgentStatusLevel(status, seenAgentRevision(status)) !== "done";
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
    occupant_exited: frame.occupantExited,
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

  const active = update.active ? AgentStatus.parse(update) : null;
  const retained = active && !releasedOccupantIsSpent(active) ? active : null;
  if (retained) {
    // Ordered before the store write: the navigation projection reads both in
    // the same recomputation.
    recordArrival(update);
    setRootStore("agent_status", retained.session_id, retained);
  } else {
    arrivalBySession.delete(update.session_id);
    deleteStoreRecord("agent_status", update.session_id);
  }
  publish({
    sessionId: update.session_id,
    previous: current,
    next: retained,
    revision: update.revision,
  });
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
  arrivalBySession.delete(sessionId);
  deleteStoreRecord("agent_status", sessionId);
  publish({ sessionId, previous: current, next: null, revision: current.revision });
}

/** Retire released occupants this profile has nothing left to show for.
 *  Acknowledgement is browser-profile state that also arrives from another tab
 *  through the shared acknowledgement store, so the sweep is driven from the
 *  acknowledgement side rather than from the frame that produced the row. */
export function retireSpentReleasedAgentStatuses(): void {
  for (const [sessionId, value] of Object.entries(rootStore.agent_status)) {
    const current = value as AgentStatusValue;
    if (!releasedOccupantIsSpent(current)) continue;
    arrivalBySession.delete(sessionId);
    deleteStoreRecord("agent_status", sessionId);
    publish({ sessionId, previous: detach(current), next: null, revision: current.revision });
  }
}

/** Drop every identity fence and notify subscribers so delayed
 * notification timers cannot outlive the resources they describe. */
export function resetAgentStatusProjection(): void {
  const currentStatuses = Object.entries(rootStore.agent_status);
  admissionBySession.clear();
  arrivalBySession.clear();
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
