// Dashboard-scoped observed-agent read handlers authorize against durable open
// session rows before consulting the coordinator's volatile status registry.
// They project an explicit public view and never depend on worker routability.

import { create } from "@bufbuild/protobuf";
import { Code, ConnectError, type ServiceImpl } from "@connectrpc/connect";
import {
  AgentStatusGetResponseSchema,
  AgentStatusListResponseSchema,
  AgentStatusViewSchema,
  CoordinatorService,
  type AgentStatusView,
} from "@roost/shared/proto/coordinator_pb";
import {
  isIdentifiedAgentStatus,
  type AgentStatus,
} from "@roost/shared/wire";
import { getAgentStatusSnapshot } from "../agent-status-hub.ts";
import { requireDashboardActor } from "./auth-interceptor.ts";
import type { ConnectDeps } from "./router.ts";

type AgentStatusMethods = "agentStatusGet" | "agentStatusList";

export type AgentStatusHandlers = Pick<
  ServiceImpl<typeof CoordinatorService>,
  AgentStatusMethods
>;

export function makeAgentStatusHandlers(deps: ConnectDeps): AgentStatusHandlers {
  return {
    async agentStatusGet(request, context) {
      const actor = requireDashboardActor(context.values);
      const session = await deps.db.selectFrom("sessions")
        .select("id")
        .where("id", "=", request.sessionId)
        .where("dashboard_id", "=", actor.dashboardId)
        .where("status", "=", "open")
        .executeTakeFirst();
      if (!session) agentStatusNotFound();

      const status = getAgentStatusSnapshot()
        .find((candidate) => candidate.session_id === session.id);
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
  };
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
