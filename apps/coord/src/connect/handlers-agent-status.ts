// Dashboard-scoped observed-agent handlers authorize durable open-session
// membership before consulting volatile status or registering bounded waits.
// They expose PID-free projections and preserve one not-found response for
// missing and foreign sessions.

import { create } from "@bufbuild/protobuf";
import { Code, ConnectError, type ServiceImpl } from "@connectrpc/connect";
import {
  AgentStatusGetResponseSchema,
  AgentStatusListResponseSchema,
  AgentStatusViewSchema,
  AgentStatusWaitResponseSchema,
  CoordinatorService,
  type AgentStatusView,
} from "@roost/shared/proto/coordinator_pb";
import {
  isIdentifiedAgentStatus,
  type AgentStatus,
} from "@roost/shared/wire";
import {
  AgentStatusWaitError,
  getAgentStatusSnapshot,
  waitForAgentStatus,
} from "../agent-status-hub.ts";
import { requireDashboardActor } from "./auth-interceptor.ts";
import type { ConnectDeps } from "./router.ts";

type AgentStatusMethods = "agentStatusGet" | "agentStatusList" | "agentStatusWait";

export type AgentStatusHandlers = Pick<
  ServiceImpl<typeof CoordinatorService>,
  AgentStatusMethods
>;

export function makeAgentStatusHandlers(deps: ConnectDeps): AgentStatusHandlers {
  return {
    async agentStatusGet(request, context) {
      const actor = requireDashboardActor(context.values);
      const sessionId = await requireOpenAgentStatusSession(
        deps,
        actor.dashboardId,
        request.sessionId,
      );
      const status = getAgentStatusSnapshot()
        .find((candidate) => candidate.session_id === sessionId);
      if (!status) agentStatusNotFound();

      return create(AgentStatusGetResponseSchema, {
        status: agentStatusView(status),
      });
    },

    async agentStatusList(_request, context) {
      const actor = requireDashboardActor(context.values);
      const sessions = await deps.db.selectFrom("sessions")
        .select("id")
        .where("dashboard_id", "=", actor.dashboardId)
        .where("status", "=", "open")
        .execute();
      const authorizedSessionIds = new Set(sessions.map((session) => session.id));
      const statuses = getAgentStatusSnapshot()
        .filter((status) => authorizedSessionIds.has(status.session_id))
        .sort(compareAgentStatusSessionIds)
        .map(agentStatusView);

      return create(AgentStatusListResponseSchema, { statuses });
    },

    async agentStatusWait(request, context) {
      const actor = requireDashboardActor(context.values);
      const sessionId = await requireOpenAgentStatusSession(
        deps,
        actor.dashboardId,
        request.sessionId,
      );
      let afterRevision: number | undefined;
      if (request.afterRevision !== undefined) {
        if (request.afterRevision > BigInt(Number.MAX_SAFE_INTEGER)) {
          throw new ConnectError(
            "invalid agent status wait request",
            Code.InvalidArgument,
          );
        }
        afterRevision = Number(request.afterRevision);
      }
      try {
        const result = await waitForAgentStatus({
          sessionId,
          statusEpoch: request.statusEpoch,
          occupantId: request.occupantId,
          desiredStates: request.desiredStates,
          afterRevision,
          timeoutMs: request.timeoutMs,
        }, context.signal);
        return create(AgentStatusWaitResponseSchema, {
          outcome: result.outcome,
        });
      } catch (error) {
        remapAgentStatusWaitError(error);
      }
    },
  };
}

async function requireOpenAgentStatusSession(
  deps: ConnectDeps,
  dashboardId: string,
  sessionId: string,
): Promise<string> {
  const session = await deps.db.selectFrom("sessions")
    .select("id")
    .where("id", "=", sessionId)
    .where("dashboard_id", "=", dashboardId)
    .where("status", "=", "open")
    .executeTakeFirst();
  if (!session) agentStatusNotFound();
  return session.id;
}

function remapAgentStatusWaitError(error: unknown): never {
  if (!(error instanceof AgentStatusWaitError)) throw error;
  const code = error.kind === "invalid"
    ? Code.InvalidArgument
    : error.kind === "capacity"
      ? Code.ResourceExhausted
      : Code.Canceled;
  throw new ConnectError(error.message, code);
}

function agentStatusNotFound(): never {
  throw new ConnectError("agent status not found", Code.NotFound);
}

function compareAgentStatusSessionIds(left: AgentStatus, right: AgentStatus): number {
  if (left.session_id < right.session_id) return -1;
  if (left.session_id > right.session_id) return 1;
  return 0;
}

function agentStatusView(status: AgentStatus): AgentStatusView {
  const identified = isIdentifiedAgentStatus(status);
  return create(AgentStatusViewSchema, {
    sessionId: status.session_id,
    agentId: status.agent_id,
    state: status.state,
    message: status.message,
    revision: BigInt(status.revision),
    completedRevision: BigInt(status.completed_revision),
    updatedAt: status.updated_at,
    active: status.active,
    ...(identified ? {
      statusEpoch: status.status_epoch,
      occupantId: status.occupant_id,
      source: status.source,
    } : {}),
    promptable: identified && status.source === "integration",
  });
}
