// Bounded event-driven waits over the coordinator's retained agent status.
// The waiter registry is module-global (one per process); every waiter reads
// session facts only through the view the status hub hands it at registration,
// so the hub stays the single owner of retained frames and closure facts.
// Advance requires a real state change, never a bare publish.

import {
  AgentOccupantId,
  AgentRuntimeState,
  SessionId,
  StatusEpoch,
  isIdentifiedAgentStatus,
  type AgentOccupantId as AgentOccupantIdValue,
  type AgentRuntimeState as AgentRuntimeStateValue,
  type AgentStatus as AgentStatusValue,
  type StatusEpoch as StatusEpochValue,
} from "@roost/shared/wire";
import { log } from "@roost/shared/log";

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
  /** Revision of the frame that matched; the floor a follow-on wait pins. */
  readonly matchedRevision?: number;
}

/** Retained-session facts owned by the status hub. */
export interface AgentStatusWaitView {
  retained(sessionId: string): AgentStatusValue | undefined;
  closed(sessionId: string): boolean;
  /** Revision at which the retained occupant's state last actually changed. */
  stateChangeRevision(sessionId: string): number;
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
  view: AgentStatusWaitView;
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

const waitersBySession = new Map<string, Set<AgentStatusWaiter>>();
let totalAgentStatusWaiters = 0;

export function registerAgentStatusWait(
  request: AgentStatusWaitRequest,
  signal: AbortSignal,
  view: AgentStatusWaitView,
): Promise<AgentStatusWaitResult> {
  const prepared = validateAgentStatusWaitRequest(request);
  if (signal.aborted) {
    return Promise.reject(new AgentStatusWaitError("canceled", "agent status wait canceled"));
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
    view,
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

/** Re-evaluate one session's waiters after a retained-status or closure change. */
export function evaluateAgentStatusWaits(sessionId: string): void {
  const waiters = waitersBySession.get(sessionId);
  if (!waiters) return;
  for (const waiter of [...waiters]) evaluateAgentStatusWaiter(waiter);
}

export function closeAgentStatusWaits(sessionId: string): void {
  const waiters = waitersBySession.get(sessionId);
  if (!waiters) return;
  for (const waiter of [...waiters]) resolveAgentStatusWaiter(waiter, "session_closed");
}

export function cancelAllAgentStatusWaits(message: string): void {
  for (const waiters of [...waitersBySession.values()]) {
    for (const waiter of [...waiters]) {
      rejectAgentStatusWaiter(waiter, new AgentStatusWaitError("canceled", message));
    }
  }
  waitersBySession.clear();
  totalAgentStatusWaiters = 0;
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

function evaluateAgentStatusWaiter(waiter: AgentStatusWaiter): void {
  if (waiter.view.closed(waiter.sessionId)) {
    resolveAgentStatusWaiter(waiter, "session_closed");
    return;
  }
  const status = waiter.view.retained(waiter.sessionId);
  if (
    !status
    || !isIdentifiedAgentStatus(status)
    || status.status_epoch !== waiter.statusEpoch
    || status.occupant_id !== waiter.occupantId
  ) {
    resolveAgentStatusWaiter(waiter, "occupant_changed");
    return;
  }
  if (!waiter.desiredStates.has(status.state)) return;
  if (waiter.afterRevision !== undefined) {
    // A revision bump alone is not progress: the worker republishes an occupant
    // when only its message or its authority source changed (an expiring
    // integration lease falls back to screen detection at the same state). A
    // settled state advances on the worker's completed_revision — the exact
    // working|blocked -> idle turn boundary — and an active state advances on
    // the revision at which that state was actually entered.
    const advance = status.state === "idle"
      ? status.completed_revision
      : waiter.view.stateChangeRevision(waiter.sessionId);
    if (advance <= waiter.afterRevision) return;
  }
  resolveAgentStatusWaiter(waiter, "matched", status.revision);
}

function resolveAgentStatusWaiter(
  waiter: AgentStatusWaiter,
  outcome: AgentStatusWaitOutcome,
  matchedRevision?: number,
): void {
  if (!removeAgentStatusWaiter(waiter)) return;
  log.debug("agent-status", "wait_resolved", {
    session_id: waiter.sessionId,
    status_epoch: waiter.statusEpoch,
    occupant_id: waiter.occupantId,
    outcome,
    matched_revision: matchedRevision,
  });
  waiter.resolve(matchedRevision === undefined ? { outcome } : { outcome, matchedRevision });
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
