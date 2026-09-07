// The coordinator's single registry of live agent status validates worker
// frames, fences occupant replacement, and owns the retained view that bounded
// status waits (agent-status-wait.ts) read. Module-global maps mean one hub per
// process; session cache and session-bus events remain the only authority for
// worker ownership and terminal closure.
import {
  AgentStatus,
  AgentStatusUpdate,
  isIdentifiedAgentStatus,
  type AgentRuntimeState as AgentRuntimeStateValue,
  type AgentStatus as AgentStatusValue,
  type AgentStatusUpdate as AgentStatusUpdateValue,
} from "@roost/shared/wire";
import { diag, signal } from "@roost/shared/diag";
import { log } from "@roost/shared/log";
import { agentStatusBus, sessionBus } from "./buses.ts";
import { getCachedSessionWorker } from "./byte-hub.ts";
import {
  acceptAgentStatusPush,
  cancelAgentStatusPush,
  configureAgentStatusPush,
  stopAgentStatusPush,
  type AgentStatusPushDeps,
} from "./agent-status-push-scheduler.ts";
import { AgentStatusOrder } from "./agent-status-order.ts";
import {
  cancelAllAgentStatusWaits,
  closeAgentStatusWaits,
  evaluateAgentStatusWaits,
  registerAgentStatusWait,
  type AgentStatusWaitRequest,
  type AgentStatusWaitResult,
  type AgentStatusWaitView,
} from "./agent-status-wait.ts";

export type AgentStatusAcceptance =
  | "accepted"
  | "invalid"
  | "stale"
  | "unknown-session"
  | "wrong-worker";

export type AgentStatusHubDeps = AgentStatusPushDeps;

const activeBySession = new Map<string, AgentStatusValue>();
const statusOrderBySession = new Map<string, AgentStatusOrder>();
const closedSessionIds = new Set<string>();
let unsubscribeSessionBus: (() => void) | undefined;

/** Register a bounded wait against this hub's retained status. */
export function waitForAgentStatus(
  request: AgentStatusWaitRequest,
  signal: AbortSignal,
): Promise<AgentStatusWaitResult> {
  return registerAgentStatusWait(request, signal, retainedStatusView);
}

/** Retained state a waiter may read; the hub alone mutates these maps. */
const retainedStatusView: AgentStatusWaitView = {
  retained: (sessionId) => activeBySession.get(sessionId),
  closed: (sessionId) => closedSessionIds.has(sessionId),
  stateChangeRevision: (sessionId) =>
    statusOrderBySession.get(sessionId)?.stateChangeRevision ?? 0,
};

/** Pre-prompt activity check: retained state for one exact pinned occupant. */
export function retainedAgentOccupantState(
  sessionId: string,
  statusEpoch: string,
  occupantId: string,
): AgentRuntimeStateValue | undefined {
  const status = activeBySession.get(sessionId);
  if (
    !status
    || !isIdentifiedAgentStatus(status)
    || status.status_epoch !== statusEpoch
    || status.occupant_id !== occupantId
  ) {
    return undefined;
  }
  return status.state;
}

/**
 * Validate and retain one status frame from an authenticated worker link.
 * Ownership comes exclusively from the coordinator's session cache; the
 * payload cannot choose which worker owns a session.
 */
export function handleWorkerAgentStatus(
  workerFp: string,
  input: unknown,
): AgentStatusAcceptance {
  const result = AgentStatusUpdate.safeParse(input);
  if (!result.success) {
    diag("agent_status.frame_dropped", { reason: "invalid", worker_fp: workerFp });
    signal("worker.protocol_violation", {
      reason: "invalid_agent_status",
      worker_fp: workerFp,
      cooldownKey: workerFp,
    });
    return "invalid";
  }
  const update = result.data;
  const owner = getCachedSessionWorker(update.session_id);
  if (!owner) {
    diag("agent_status.frame_dropped", {
      reason: "unknown_session",
      worker_fp: workerFp,
      session_id: update.session_id,
    });
    return "unknown-session";
  }
  if (owner.worker_fp !== workerFp) {
    log.warn("agent-status", "worker_mismatch", {
      worker_fp: workerFp,
      owner_worker_fp: owner.worker_fp,
      session_id: update.session_id,
    });
    signal("worker.protocol_violation", {
      reason: "agent_status_worker_mismatch",
      worker_fp: workerFp,
      cooldownKey: workerFp,
    });
    return "wrong-worker";
  }

  if (closedSessionIds.has(update.session_id)) return "stale";
  const previous = activeBySession.get(update.session_id);
  const order = statusOrderBySession.get(update.session_id) ?? new AgentStatusOrder();
  if (!order.accepts(previous, update)) return "stale";

  order.record(update);
  statusOrderBySession.set(update.session_id, order);
  if (update.active) {
    activeBySession.set(update.session_id, AgentStatus.parse(update));
  } else {
    activeBySession.delete(update.session_id);
  }
  evaluateAgentStatusWaits(update.session_id);
  agentStatusBus.publish(update);
  acceptAgentStatusPush(
    previous,
    update,
    (sessionId) => activeBySession.get(sessionId),
  );
  return "accepted";
}

function clearClosedSession(sessionId: string): void {
  closedSessionIds.add(sessionId);
  closeAgentStatusWaits(sessionId);
  cancelAgentStatusPush(sessionId);
  const current = activeBySession.get(sessionId);
  activeBySession.delete(sessionId);
  if (!current) return;

  const inactive: AgentStatusUpdateValue = {
    ...current,
    active: false,
    revision: Math.min(Number.MAX_SAFE_INTEGER, current.revision + 1),
    updated_at: Math.max(Date.now(), current.updated_at),
  };
  const order = statusOrderBySession.get(sessionId) ?? new AgentStatusOrder();
  order.recordClose(current, inactive.revision);
  statusOrderBySession.set(sessionId, order);
  agentStatusBus.publish(inactive);
}

export function startAgentStatusHub(deps?: AgentStatusHubDeps): void {
  configureAgentStatusPush(deps);
  if (unsubscribeSessionBus) return;
  unsubscribeSessionBus = sessionBus.subscribe((event) => {
    if (event.kind === "closed") {
      clearClosedSession(event.session_id);
    } else if (event.kind === "opened") {
      closedSessionIds.delete(event.session_id);
    }
  });
}

export function stopAgentStatusHub(): void {
  cancelAllAgentStatusWaits("agent status hub stopped");
  stopAgentStatusPush();
  unsubscribeSessionBus?.();
  unsubscribeSessionBus = undefined;
  activeBySession.clear();
  statusOrderBySession.clear();
  closedSessionIds.clear();
}

export function getAgentStatusSnapshot(): AgentStatusValue[] {
  return [...activeBySession.values()];
}
