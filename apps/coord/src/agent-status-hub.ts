// The coordinator's single registry of live agent status: validates each
// worker frame, fences replacements by observed occupant identity, and
// debounces transition pushes by 1s instead of firing per update.
// Module-global Maps mean one hub per process; ownership comes exclusively
// from the coordinator's session cache. Retired identity tombstones survive
// close/open boundaries, and their UUIDs are never ordered by text.
import {
  AgentStatus,
  AgentStatusUpdate,
  isIdentifiedAgentStatus,
  type AgentOccupantId,
  type AgentStatus as AgentStatusValue,
  type AgentStatusUpdate as AgentStatusUpdateValue,
  type StatusEpoch,
} from "@roost/shared/wire";
import { diag, signal } from "@roost/shared/diag";
import { log } from "@roost/shared/log";
import { agentStatusBus, sessionBus } from "./buses.ts";
import { getCachedSessionWorker } from "./byte-hub.ts";
import type { KyselyDB } from "./db/connection.ts";
import {
  firePushForTransition,
  type AgentPushTransition,
  type PushTransition,
} from "./push-dispatch.ts";
import {
  AgentStatusOrder,
  sameAgentStatusOccupant,
} from "./agent-status-order.ts";

export type AgentStatusAcceptance =
  | "accepted"
  | "invalid"
  | "stale"
  | "unknown-session"
  | "wrong-worker";

export interface AgentStatusHubDeps {
  db: KyselyDB;
  pushAllowedOrigins: readonly string[];
  tenantRouteKey?: string;
  dispatchPush?: typeof firePushForTransition;
}

interface PendingPush {
  currentRevision: number;
  triggerRevision: number;
  statusEpoch: StatusEpoch;
  occupantId: AgentOccupantId;
  kind: PushTransition;
  timer: Timer;
}

const PUSH_DELAY_MS = 1_000;

const activeBySession = new Map<string, AgentStatusValue>();
const statusOrderBySession = new Map<string, AgentStatusOrder>();
const closedSessionIds = new Set<string>();
let unsubscribeSessionBus: (() => void) | undefined;
const pendingPushBySession = new Map<string, PendingPush>();
let pushDeps: AgentStatusHubDeps | undefined;

function cancelPendingPush(sessionId: string): PendingPush | undefined {
  const pending = pendingPushBySession.get(sessionId);
  if (!pending) return undefined;
  clearTimeout(pending.timer);
  pendingPushBySession.delete(sessionId);
  return pending;
}

function pendingMatchesOccupant(
  pending: PendingPush,
  status: AgentStatusValue | AgentStatusUpdateValue,
): boolean {
  return isIdentifiedAgentStatus(status)
    && pending.statusEpoch === status.status_epoch
    && pending.occupantId === status.occupant_id;
}

function pendingMatchesCurrentStatus(
  pending: PendingPush,
  status: AgentStatusValue | undefined,
): boolean {
  if (
    !status
    || status.revision !== pending.currentRevision
    || !pendingMatchesOccupant(pending, status)
  ) return false;
  return pending.kind === "blocked" ? status.state === "blocked" : status.state === "idle";
}

function classifyTransition(
  previous: AgentStatusValue | undefined,
  next: AgentStatusUpdateValue,
): PushTransition | undefined {
  if (!next.active || !previous || !sameAgentStatusOccupant(previous, next)) return undefined;
  if (previous.state === "working" && next.state === "blocked") return "blocked";
  if (
    (previous.state === "working" || previous.state === "blocked")
    && next.state === "idle"
    && next.completed_revision > previous.completed_revision
  ) return "done";
  return undefined;
}

function schedulePush(
  previous: AgentStatusValue | undefined,
  next: AgentStatusUpdateValue,
  carried: PendingPush | undefined,
): void {
  if (
    !pushDeps
    || pushDeps.pushAllowedOrigins.length === 0
    || !next.active
    || !isIdentifiedAgentStatus(next)
  ) return;
  let kind = classifyTransition(previous, next);
  let triggerRevision = next.revision;
  if (
    !kind
    && carried
    && pendingMatchesOccupant(carried, next)
    && (
      (carried.kind === "blocked" && next.state === "blocked")
      || (carried.kind === "done" && next.state === "idle")
    )
  ) {
    kind = carried.kind;
    triggerRevision = carried.triggerRevision;
  }
  if (!kind) return;

  const sessionId = next.session_id;
  const currentRevision = next.revision;
  const transitionKind = kind;
  let pending: PendingPush;
  const timer = setTimeout(() => {
    if (pendingPushBySession.get(sessionId) !== pending) return;
    pendingPushBySession.delete(sessionId);
    if (!pendingMatchesCurrentStatus(pending, activeBySession.get(sessionId))) return;
    const deps = pushDeps;
    const dispatch = deps?.dispatchPush ?? firePushForTransition;
    if (!deps || !dispatch) return;
    const transition: AgentPushTransition = {
      sessionId,
      kind: transitionKind,
      statusEpoch: pending.statusEpoch,
      occupantId: pending.occupantId,
      revision: pending.triggerRevision,
    };
    void dispatch(
      deps.db,
      transition,
      deps.pushAllowedOrigins,
      () => pendingMatchesCurrentStatus(pending, activeBySession.get(sessionId)),
      undefined,
      deps.tenantRouteKey,
    ).catch((error) => {
      log.warn("agent-status", "push_failed", {
        session_id: sessionId,
        kind: transitionKind,
        status_epoch: pending.statusEpoch,
        occupant_id: pending.occupantId,
        revision: pending.triggerRevision,
        error: String(error),
      });
    });
  }, PUSH_DELAY_MS);
  timer.unref?.();
  pending = {
    currentRevision,
    triggerRevision,
    kind: transitionKind,
    timer,
    statusEpoch: next.status_epoch,
    occupantId: next.occupant_id,
  };
  pendingPushBySession.set(sessionId, pending);
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

  const carriedPush = cancelPendingPush(update.session_id);
  order.record(update);
  statusOrderBySession.set(update.session_id, order);
  if (update.active) {
    activeBySession.set(update.session_id, AgentStatus.parse(update));
  } else {
    activeBySession.delete(update.session_id);
  }
  agentStatusBus.publish(update);
  schedulePush(previous, update, carriedPush);
  return "accepted";
}

function clearClosedSession(sessionId: string): void {
  cancelPendingPush(sessionId);
  const current = activeBySession.get(sessionId);
  activeBySession.delete(sessionId);
  closedSessionIds.add(sessionId);
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
  if (deps) pushDeps = deps;
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
  for (const pending of pendingPushBySession.values()) clearTimeout(pending.timer);
  pendingPushBySession.clear();
  pushDeps = undefined;
  unsubscribeSessionBus?.();
  unsubscribeSessionBus = undefined;
  activeBySession.clear();
  statusOrderBySession.clear();
  closedSessionIds.clear();
}

export function getAgentStatusSnapshot(): AgentStatusValue[] {
  return [...activeBySession.values()];
}
