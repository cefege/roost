// Schedules debounced Web Push delivery for accepted agent-status transitions.
// The status hub supplies current occupant lookup so this module never owns
// live status; coordinator startup provides persistence and delivery deps.

import { isIdentifiedAgentStatus } from "@roost/shared/wire";
import type {
  AgentOccupantId,
  AgentStatus,
  AgentStatusUpdate,
  StatusEpoch,
} from "@roost/shared/wire";
import { log } from "@roost/shared/log";
import type { KyselyDB } from "./db/connection.ts";
import {
  firePushForTransition,
  type AgentPushTransition,
  type PushTransition,
} from "./push-dispatch.ts";
import { sameAgentStatusOccupant } from "./agent-status-order.ts";

export interface AgentStatusPushDeps {
  db: KyselyDB;
  pushAllowedOrigins: readonly string[];
  dispatchPush?: typeof firePushForTransition;
}

type CurrentStatus = (sessionId: string) => AgentStatus | undefined;

interface PendingPush {
  currentRevision: number;
  triggerRevision: number;
  statusEpoch: StatusEpoch;
  occupantId: AgentOccupantId;
  kind: PushTransition;
  timer: Timer;
}

const PUSH_DELAY_MS = 1_000;
const pendingPushBySession = new Map<string, PendingPush>();
let pushDeps: AgentStatusPushDeps | undefined;

export function configureAgentStatusPush(deps?: AgentStatusPushDeps): void {
  if (deps) pushDeps = deps;
}

export function acceptAgentStatusPush(
  previous: AgentStatus | undefined,
  next: AgentStatusUpdate,
  currentStatus: CurrentStatus,
): void {
  schedulePush(previous, next, cancelPendingPush(next.session_id), currentStatus);
}

export function cancelAgentStatusPush(sessionId: string): void {
  cancelPendingPush(sessionId);
}

export function stopAgentStatusPush(): void {
  for (const pending of pendingPushBySession.values()) clearTimeout(pending.timer);
  pendingPushBySession.clear();
  pushDeps = undefined;
}

function cancelPendingPush(sessionId: string): PendingPush | undefined {
  const pending = pendingPushBySession.get(sessionId);
  if (!pending) return undefined;
  clearTimeout(pending.timer);
  pendingPushBySession.delete(sessionId);
  return pending;
}

function pendingMatchesOccupant(
  pending: PendingPush,
  status: AgentStatus | AgentStatusUpdate,
): boolean {
  return isIdentifiedAgentStatus(status)
    && pending.statusEpoch === status.status_epoch
    && pending.occupantId === status.occupant_id;
}

function pendingMatchesCurrentStatus(
  pending: PendingPush,
  status: AgentStatus | undefined,
): boolean {
  if (
    !status
    || status.revision !== pending.currentRevision
    || !pendingMatchesOccupant(pending, status)
  ) return false;
  return pending.kind === "blocked" ? status.state === "blocked" : status.state === "idle";
}

function classifyTransition(
  previous: AgentStatus | undefined,
  next: AgentStatusUpdate,
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
  previous: AgentStatus | undefined,
  next: AgentStatusUpdate,
  carried: PendingPush | undefined,
  currentStatus: CurrentStatus,
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
    if (!pendingMatchesCurrentStatus(pending, currentStatus(sessionId))) return;
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
      () => pendingMatchesCurrentStatus(pending, currentStatus(sessionId)),
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
