// Agent-session RPC handlers: answer an omp prompt, abort the current turn, and
// page the durable transcript. Respond/abort route to the owning worker;
// history reads coord SQLite and remains available while the worker is offline.

import type { ServiceImpl } from "@connectrpc/connect";
import { Code, ConnectError } from "@connectrpc/connect";
import { create } from "@bufbuild/protobuf";
import { randomUUID } from "node:crypto";
import {
  CoordinatorService,
  SessionsAgentRespondResponseSchema, SessionsAgentAbortResponseSchema,
  SessionsGetAgentEntriesResponseSchema,
} from "@roost/shared/proto/coordinator_pb";
import { agentEntryToProto } from "@roost/shared/wire/agent-proto";
import { asSessionId } from "@roost/shared/wire";
import { requireAuth } from "./auth-interceptor.ts";
import { forwardToSessionWorker } from "./router-helpers.ts";
import type { ConnectDeps } from "./router.ts";
import { pageAgentEntries } from "../agent-transcript.ts";

type AgentSessionMethods =
  | "sessionsAgentRespond" | "sessionsAgentAbort" | "sessionsGetAgentEntries";


export function makeAgentSessionHandlers(
  deps: ConnectDeps,
): Pick<ServiceImpl<typeof CoordinatorService>, AgentSessionMethods> {
  return {
    // Answer an omp prompt (approval / question / input). Fire-and-ack: an
    // unanswered prompt hangs the agent forever, so the reply goes out
    // immediately and the AUTHORITATIVE prompt state comes back as the same
    // AgentEntry re-emitted with state answered/cancelled on the transcript
    // stream. accepted:false means "never reached the worker" — a stale or
    // already-answered prompt_id is the worker's call, not ours.
    async sessionsAgentRespond(req, ctx) {
      const caller = requireAuth(ctx.values);
      const ok = await forwardToSessionWorker(deps.db, req.sessionId, caller, {
        kind: "agent-respond" as const,
        request_id: randomUUID(),
        session_id: asSessionId(req.sessionId),
        prompt_id: req.promptId,
        value: req.value,
        cancelled: req.cancelled,
      });
      return create(SessionsAgentRespondResponseSchema, { accepted: ok });
    },

    async sessionsAgentAbort(req, ctx) {
      const caller = requireAuth(ctx.values);
      const ok = await forwardToSessionWorker(deps.db, req.sessionId, caller, {
        kind: "omp-abort" as const,
        request_id: randomUUID(),
        session_id: asSessionId(req.sessionId),
      });
      return create(SessionsAgentAbortResponseSchema, { accepted: ok });
    },

    // Paged durable transcript. The sessions lookup preserves a loud 404 for a
    // bad id; a disconnected worker is irrelevant because coord owns history.
    async sessionsGetAgentEntries(req, ctx) {
      requireAuth(ctx.values);
      const row = await deps.db
        .selectFrom("sessions")
        .select("id")
        .where("id", "=", req.sessionId)
        .executeTakeFirst();
      if (!row) throw new ConnectError("unknown session", Code.NotFound);
      const page = pageAgentEntries(deps.sqlite, req.sessionId, Number(req.beforeSeq), 512);
      return create(SessionsGetAgentEntriesResponseSchema, {
        entries: page.entries.map(agentEntryToProto),
        firstSeq: BigInt(page.first_seq),
        more: page.more,
      });
    },
  };
}
