// The coordinator's single registry of live agent status validates worker
// frames, fences occupant replacement, and owns bounded event-driven waits.
// Module-global maps mean one hub per process; session cache and session-bus
// events remain the only authority for worker ownership and terminal closure.
import {
  AgentOccupantId,
  AgentRuntimeState,
  AgentStatus,
  AgentStatusUpdate,
  SessionId,
  StatusEpoch,
  isIdentifiedAgentStatus,
  type AgentOccupantId as AgentOccupantIdValue,
  type AgentRuntimeState as AgentRuntimeStateValue,
  type AgentStatus as AgentStatusValue,
  type AgentStatusUpdate as AgentStatusUpdateValue,
  type StatusEpoch as StatusEpochValue,
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

export type AgentStatusAcceptance =
  | "accepted"
  | "invalid"
  | "stale"
  | "unknown-session"
  | "wrong-worker";

export type AgentStatusHubDeps = AgentStatusPushDeps;
export type AgentStatusWaitOutcome =
  | "matched"
  | "timed_out"
  | "occupant_changed"
  | "session_closed";

export interface AgentStatusWaitRequest {
  readonly sessionId: string;
  readonly statusEpoch: string;
  readonly occupantId: string;
  readonly desiredStates: readonly string[];
  readonly afterRevision?: number;
  readonly timeoutMs: number;
}

export interface AgentStatusWaitResult {
  readonly outcome: AgentStatusWaitOutcome;
}

export type AgentStatusWaitErrorKind = "invalid" | "capacity" | "canceled";

export class AgentStatusWaitError extends Error {
  constructor(
    readonly kind: AgentStatusWaitErrorKind,
    message: string,
    readonly capacity?: "session" | "global",
  ) {
    super(message);
    this.name = "AgentStatusWaitError";
  }
}

interface PreparedAgentStatusWaitRequest {
  sessionId: string;
  statusEpoch: StatusEpochValue;
  occupantId: AgentOccupantIdValue;
  desiredStates: Set<AgentRuntimeStateValue>;
  afterRevision?: number;
  timeoutMs: number;
}

interface AgentStatusWaiter extends PreparedAgentStatusWaitRequest {
  signal: AbortSignal;
  abortListener: () => void;
  timer?: Timer;
  settled: boolean;
  resolve: (result: AgentStatusWaitResult) => void;
  reject: (error: AgentStatusWaitError) => void;
}

export const AGENT_STATUS_WAIT_MAX_TIMEOUT_MS = 300_000;
export const AGENT_STATUS_WAIT_MAX_PER_SESSION = 32;
export const AGENT_STATUS_WAIT_MAX_GLOBAL = 2_048;

const activeBySession = new Map<string, AgentStatusValue>();
const statusOrderBySession = new Map<string, AgentStatusOrder>();
const closedSessionIds = new Set<string>();
const waitersBySession = new Map<string, Set<AgentStatusWaiter>>();
let totalAgentStatusWaiters = 0;
let unsubscribeSessionBus: (() => void) | undefined;

export async function waitForAgentStatus(
  request: AgentStatusWaitRequest,
  signal: AbortSignal,
): Promise<AgentStatusWaitResult> {
  const prepared = validateAgentStatusWaitRequest(request);
  if (signal.aborted) {
    throw new AgentStatusWaitError("canceled", "agent status wait canceled");
  }
  const existing = waitersBySession.get(prepared.sessionId);
  if ((existing?.size ?? 0) >= AGENT_STATUS_WAIT_MAX_PER_SESSION) {
    throw new AgentStatusWaitError(
      "capacity",
      "agent status wait capacity exhausted",
      "session",
    );
  }
  if (totalAgentStatusWaiters >= AGENT_STATUS_WAIT_MAX_GLOBAL) {
    throw new AgentStatusWaitError(
      "capacity",
      "agent status wait capacity exhausted",
      "global",
    );
  }

  const { promise, resolve, reject } = Promise.withResolvers<AgentStatusWaitResult>();
  const waiter: AgentStatusWaiter = {
    ...prepared,
    signal,
    settled: false,
    abortListener: () => rejectAgentStatusWaiter(
      waiter,
      new AgentStatusWaitError("canceled", "agent status wait canceled"),
    ),
    resolve,
    reject,
  };
  const sessionWaiters = existing ?? new Set<AgentStatusWaiter>();
  if (!existing) waitersBySession.set(prepared.sessionId, sessionWaiters);
  sessionWaiters.add(waiter);
  totalAgentStatusWaiters += 1;
  signal.addEventListener("abort", waiter.abortListener, { once: true });
  const timer = setTimeout(
    () => resolveAgentStatusWaiter(waiter, "timed_out"),
    prepared.timeoutMs,
  );
  timer.unref?.();
  waiter.timer = timer;
  log.debug("agent-status", "wait_registered", {
    session_id: prepared.sessionId,
    status_epoch: prepared.statusEpoch,
    occupant_id: prepared.occupantId,
  });

  if (signal.aborted) waiter.abortListener();
  else evaluateAgentStatusWaiter(waiter);
  return promise;
}

export function _agentStatusWaiterStats(): {
  total: number;
  sessions: number;
} {
  return {
    total: totalAgentStatusWaiters,
    sessions: waitersBySession.size,
  };
}

function validateAgentStatusWaitRequest(
  request: AgentStatusWaitRequest,
): PreparedAgentStatusWaitRequest {
  const sessionId = SessionId.safeParse(request.sessionId);
  const statusEpoch = StatusEpoch.safeParse(request.statusEpoch);
  const occupantId = AgentOccupantId.safeParse(request.occupantId);
  const desiredStates = request.desiredStates.map((state) => AgentRuntimeState.safeParse(state));
  const uniqueStates = new Set(request.desiredStates);
  if (
    !sessionId.success
    || !statusEpoch.success
    || !occupantId.success
    || request.desiredStates.length === 0
    || uniqueStates.size !== request.desiredStates.length
    || desiredStates.some((state) => !state.success)
    || !Number.isSafeInteger(request.timeoutMs)
    || request.timeoutMs < 1
    || request.timeoutMs > AGENT_STATUS_WAIT_MAX_TIMEOUT_MS
    || (
      request.afterRevision !== undefined
      && (
        !Number.isSafeInteger(request.afterRevision)
        || request.afterRevision < 0
      )
    )
  ) {
    throw new AgentStatusWaitError("invalid", "invalid agent status wait request");
  }
  return {
    sessionId: sessionId.data,
    statusEpoch: statusEpoch.data,
    occupantId: occupantId.data,
    desiredStates: new Set(desiredStates.map((state) => state.data!)),
    afterRevision: request.afterRevision,
    timeoutMs: request.timeoutMs,
  };
}

function evaluateAgentStatusWaiters(sessionId: string): void {
  const waiters = waitersBySession.get(sessionId);
  if (!waiters) return;
  for (const waiter of [...waiters]) evaluateAgentStatusWaiter(waiter);
}

function evaluateAgentStatusWaiter(waiter: AgentStatusWaiter): void {
  if (closedSessionIds.has(waiter.sessionId)) {
    resolveAgentStatusWaiter(waiter, "session_closed");
    return;
  }
  const status = activeBySession.get(waiter.sessionId);
  if (
    !status
    || !isIdentifiedAgentStatus(status)
    || status.status_epoch !== waiter.statusEpoch
    || status.occupant_id !== waiter.occupantId
  ) {
    resolveAgentStatusWaiter(waiter, "occupant_changed");
    return;
  }
  if (
    waiter.desiredStates.has(status.state)
    && (waiter.afterRevision === undefined || status.revision > waiter.afterRevision)
  ) {
    resolveAgentStatusWaiter(waiter, "matched");
  }
}

function resolveAgentStatusWaiter(
  waiter: AgentStatusWaiter,
  outcome: AgentStatusWaitOutcome,
): void {
  if (!removeAgentStatusWaiter(waiter)) return;
  log.debug("agent-status", "wait_resolved", {
    session_id: waiter.sessionId,
    status_epoch: waiter.statusEpoch,
    occupant_id: waiter.occupantId,
    outcome,
  });
  waiter.resolve({ outcome });
}

function rejectAgentStatusWaiter(
  waiter: AgentStatusWaiter,
  error: AgentStatusWaitError,
): void {
  if (!removeAgentStatusWaiter(waiter)) return;
  log.debug("agent-status", "wait_rejected", {
    session_id: waiter.sessionId,
    status_epoch: waiter.statusEpoch,
    occupant_id: waiter.occupantId,
    reason: error.kind,
  });
  waiter.reject(error);
}

function removeAgentStatusWaiter(waiter: AgentStatusWaiter): boolean {
  if (waiter.settled) return false;
  waiter.settled = true;
  clearTimeout(waiter.timer);
  waiter.signal.removeEventListener("abort", waiter.abortListener);
  const sessionWaiters = waitersBySession.get(waiter.sessionId);
  sessionWaiters?.delete(waiter);
  if (sessionWaiters?.size === 0) waitersBySession.delete(waiter.sessionId);
  totalAgentStatusWaiters -= 1;
  return true;
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
  evaluateAgentStatusWaiters(update.session_id);
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
  const waiters = waitersBySession.get(sessionId);
  if (waiters) {
    for (const waiter of [...waiters]) {
      resolveAgentStatusWaiter(waiter, "session_closed");
    }
  }
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
  for (const waiters of [...waitersBySession.values()]) {
    for (const waiter of [...waiters]) {
      rejectAgentStatusWaiter(
        waiter,
        new AgentStatusWaitError("canceled", "agent status hub stopped"),
      );
    }
  }
  waitersBySession.clear();
  totalAgentStatusWaiters = 0;
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
