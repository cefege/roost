// The coordinator's single registry of live agent status: validates each
// worker status frame, tracks the newest accepted revision per session, and
// debounces transition pushes by 1s instead of firing per update.
// Module-global Maps mean one hub per process; ownership comes exclusively
// from the coordinator's session cache — a payload can never claim a session
// for a worker that doesn't already own it, and stale revisions never win.
import {
  AgentStatus,
  AgentStatusUpdate,
  type AgentStatus as AgentStatusValue,
  type AgentStatusUpdate as AgentStatusUpdateValue,
} from "@roost/shared/wire";
import { diag, signal } from "@roost/shared/diag";
import { log } from "@roost/shared/log";
import { agentStatusBus, sessionBus } from "./buses.ts";
import { getCachedSessionWorker } from "./byte-hub.ts";
import type { KyselyDB } from "./db/connection.ts";
import { firePushForTransition, type PushTransition } from "./push-dispatch.ts";

export type AgentStatusAcceptance =
  | "accepted"
  | "invalid"
  | "stale"
  | "unknown-session"
  | "wrong-worker";

export interface AgentStatusHubDeps {
  db: KyselyDB;
  dispatchPush?: typeof firePushForTransition;
}

interface PendingPush {
  revision: number;
  kind: PushTransition;
  timer: ReturnType<typeof setTimeout>;
}

const PUSH_DELAY_MS = 1_000;

const activeBySession = new Map<string, AgentStatusValue>();
const latestRevisionBySession = new Map<string, number>();
let unsubscribeSessionBus: (() => void) | undefined;
const pendingPushBySession = new Map<string, PendingPush>();
let pushDeps: AgentStatusHubDeps | undefined;

function cancelPendingPush(sessionId: string): PushTransition | undefined {
  const pending = pendingPushBySession.get(sessionId);
  if (!pending) return undefined;
  clearTimeout(pending.timer);
  pendingPushBySession.delete(sessionId);
  return pending.kind;
}

function classifyTransition(
  previous: AgentStatusValue | undefined,
  next: AgentStatusUpdateValue,
): PushTransition | undefined {
  if (!next.active || !previous) return undefined;
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
  carriedKind: PushTransition | undefined,
): void {
  if (!pushDeps || !next.active) return;
  let kind = classifyTransition(previous, next);
  if (
    !kind
    && carriedKind === "blocked"
    && next.state === "blocked"
  ) kind = carriedKind;
  if (
    !kind
    && carriedKind === "done"
    && next.state === "idle"
  ) kind = carriedKind;
  if (!kind) return;

  const sessionId = next.session_id;
  const revision = next.revision;
  const timer = setTimeout(() => {
    pendingPushBySession.delete(sessionId);
    const current = activeBySession.get(sessionId);
    if (!current || current.revision !== revision) return;
    if (kind === "blocked" && current.state !== "blocked") return;
    if (kind === "done" && current.state !== "idle") return;
    const dispatch = pushDeps?.dispatchPush ?? firePushForTransition;
    if (!pushDeps || !dispatch) return;
    void dispatch(pushDeps.db, sessionId, kind).catch((error) => {
      log.warn("agent-status", "push_failed", {
        session_id: sessionId,
        kind,
        error: String(error),
      });
    });
  }, PUSH_DELAY_MS);
  timer.unref?.();
  pendingPushBySession.set(sessionId, { revision, kind, timer });
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

  const latestRevision = latestRevisionBySession.get(update.session_id) ?? -1;
  if (update.revision <= latestRevision) return "stale";
  const previous = activeBySession.get(update.session_id);
  const carriedPushKind = cancelPendingPush(update.session_id);
  latestRevisionBySession.set(update.session_id, update.revision);
  if (update.active) {
    activeBySession.set(update.session_id, AgentStatus.parse(update));
  } else {
    activeBySession.delete(update.session_id);
  }
  agentStatusBus.publish(update);
  schedulePush(previous, update, carriedPushKind);
  return "accepted";
}

function clearClosedSession(sessionId: string): void {
  cancelPendingPush(sessionId);
  const current = activeBySession.get(sessionId);
  activeBySession.delete(sessionId);
  latestRevisionBySession.delete(sessionId);
  if (!current) return;

  const inactive: AgentStatusUpdateValue = {
    ...current,
    active: false,
    revision: Math.min(Number.MAX_SAFE_INTEGER, current.revision + 1),
    updated_at: Math.max(Date.now(), current.updated_at),
  };
  agentStatusBus.publish(inactive);
}

export function startAgentStatusHub(deps?: AgentStatusHubDeps): void {
  if (deps) pushDeps = deps;
  if (unsubscribeSessionBus) return;
  unsubscribeSessionBus = sessionBus.subscribe((event) => {
    if (event.kind === "closed") clearClosedSession(event.session_id);
  });
}

export function stopAgentStatusHub(): void {
  for (const pending of pendingPushBySession.values()) clearTimeout(pending.timer);
  pendingPushBySession.clear();
  pushDeps = undefined;
  unsubscribeSessionBus?.();
  unsubscribeSessionBus = undefined;
  activeBySession.clear();
  latestRevisionBySession.clear();
}

export function getAgentStatusSnapshot(): AgentStatusValue[] {
  return [...activeBySession.values()];
}

export function getAgentStatusDiagnostics(): {
  active: number;
  revision_floors: number;
  subscribers: number;
  pending_pushes: number;
} {
  return {
    active: activeBySession.size,
    revision_floors: latestRevisionBySession.size,
    subscribers: agentStatusBus.subscriberCount,
    pending_pushes: pendingPushBySession.size,
  };
}
