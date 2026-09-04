// Volatile coding-agent status projection. Sync frames are worker-revisioned;
// this module retains a per-session floor even after an inactive deletion so a
// delayed frame cannot resurrect stale state.

import {
  AgentStatus,
  AgentStatusUpdate,
  type AgentStatus as AgentStatusValue,
} from "@roost/shared/wire";
import type { AgentStatusFrame } from "@roost/shared/proto/sync_pb";
import { signal } from "@roost/shared/diag";
import { deleteStoreRecord, rootStore, setRootStore } from "./root.ts";

export interface AgentStatusChange {
  sessionId: string;
  previous: AgentStatusValue | null;
  next: AgentStatusValue | null;
  revision: number;
}

const revisionFloors = new Map<string, number>();
const subscribers = new Set<(change: AgentStatusChange) => void>();

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

/** Store reads return live proxies: a subscriber would otherwise see the
 *  POST-update value as `previous` and every transition would look like a
 *  self-transition. Every field is primitive, so a shallow copy detaches it. */
function detach(current: AgentStatusValue | undefined): AgentStatusValue | null {
  return current ? { ...current } : null;
}

/** Validate, order, and project one Sync AgentStatusFrame. */
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
  });
  if (!parsed.success) {
    signal("diag.corruption_signal", {
      kind: "invalid_agent_status",
      sid: frame.sessionId,
      cooldownKey: frame.sessionId,
    });
    return false;
  }

  const update = parsed.data;
  const current = detach(rootStore.agent_status[update.session_id] as AgentStatusValue | undefined);
  const floor = revisionFloors.get(update.session_id) ?? current?.revision ?? -1;
  if (update.revision <= floor) return false;
  revisionFloors.set(update.session_id, update.revision);

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
    if (current) {
      deleteStoreRecord("agent_status", update.session_id);
      publish({
        sessionId: update.session_id,
        previous: current,
        next: null,
        revision: update.revision,
      });
    }
  }
  return true;
}

/** Remove volatile status when the durable session projector removes a row. */
export function clearAgentStatusForSession(sessionId: string): void {
  const current = detach(rootStore.agent_status[sessionId] as AgentStatusValue | undefined);
  if (!current) return;
  revisionFloors.set(
    sessionId,
    Math.max(revisionFloors.get(sessionId) ?? -1, current.revision),
  );
  deleteStoreRecord("agent_status", sessionId);
  publish({ sessionId, previous: current, next: null, revision: current.revision });
}

/** Drop every dashboard's revision floors and notify subscribers so delayed
 * notification timers cannot outlive the resources they describe. */
export function resetAgentStatusProjection(): void {
  const currentStatuses = Object.entries(rootStore.agent_status);
  revisionFloors.clear();
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
