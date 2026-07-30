// Correlated SessionSurface actions for kind="agent" sessions. Commands are
// validated at coord and worker trust boundaries, then forwarded to the one OMP
// child that owns the session; response.data is preserved as JSON.

import { create } from "@bufbuild/protobuf";
import { Code, ConnectError, type ServiceImpl } from "@connectrpc/connect";
import {
  CoordinatorService,
  SessionsAgentUiCommandResponseSchema,
} from "@roost/shared/proto/coordinator_pb";
import {
  asSessionId,
  parseAgentUiRpcCommandJson,
  type AgentUiRpcCommand,
} from "@roost/shared/wire";
import { createPendingRpc } from "../router/pending-rpcs.ts";
import { requireAuth } from "./auth-interceptor.ts";
import { sendBrowserCmd } from "./router-helpers.ts";
import type { ConnectDeps } from "./router.ts";
import { getWorkerHubSocket } from "./worker-service.ts";

type AgentUiCommandMethods = "sessionsAgentUiCommand";

export function makeAgentUiCommandHandlers(
  deps: ConnectDeps,
): Pick<ServiceImpl<typeof CoordinatorService>, AgentUiCommandMethods> {
  return {
    async sessionsAgentUiCommand(req, ctx) {
      const caller = requireAuth(ctx.values);
      let command: AgentUiRpcCommand;
      try {
        command = parseAgentUiRpcCommandJson(req.commandJson);
      } catch (err) {
        throw new ConnectError(
          err instanceof Error ? err.message : String(err),
          Code.InvalidArgument,
        );
      }

      if (!req.sessionId) {
        throw new ConnectError("session_id is required", Code.InvalidArgument);
      }
      const row = await deps.db
        .selectFrom("sessions")
        .select(["worker_fp", "kind"])
        .where("id", "=", req.sessionId)
        .executeTakeFirst();
      if (!row) throw new ConnectError("session not found", Code.NotFound);
      if (row.kind !== "agent") {
        throw new ConnectError("session is not an agent", Code.FailedPrecondition);
      }
      const sock = getWorkerHubSocket(row.worker_fp);
      if (!sock) throw new ConnectError("worker offline", Code.Unavailable);

      const pending = createPendingRpc<unknown>(
        command.type === "subagent_command" ? 310_000 : 35_000,
        row.worker_fp,
      );
      sendBrowserCmd(sock, caller, pending.request_id, {
        kind: "agent-ui-command",
        request_id: pending.request_id,
        session_id: asSessionId(req.sessionId),
        command_json: req.commandJson,
      });
      const result = await pending.promise;
      if (typeof result !== "object" || result === null || Array.isArray(result)) {
        throw new ConnectError("worker returned an invalid agent UI command response", Code.Internal);
      }
      const fields = result as Record<string, unknown>;
      if (typeof fields.accepted !== "boolean" || typeof fields.data_json !== "string") {
        throw new ConnectError("worker returned an invalid agent UI command response", Code.Internal);
      }
      return create(SessionsAgentUiCommandResponseSchema, {
        accepted: fields.accepted,
        dataJson: fields.data_json,
      });
    },
  };
}
