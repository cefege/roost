// Agent-session RPC handlers: answer an omp prompt, abort the current turn, and
// page the transcript. Sibling of handlers-sessions.ts (400-line cap) and spread
// into the same router.service() literal; the shell-only session handlers stay
// there and nothing here touches a PTY.
//
// Every one of these routes to the worker that owns the session, which owns the
// `omp --mode rpc-ui` child. Respond/abort are fire-and-ack (the authoritative
// state comes back on the transcript stream); get-agent-entries is a
// request/reply round-trip through the pending-rpc table.

import type { ServiceImpl } from "@connectrpc/connect";
import { Code, ConnectError } from "@connectrpc/connect";
import { create } from "@bufbuild/protobuf";
import { randomUUID } from "node:crypto";
import {
  CoordinatorService,
  SessionsAgentRespondResponseSchema, SessionsAgentAbortResponseSchema,
  SessionsGetAgentEntriesResponseSchema,
} from "@roost/shared/proto/coordinator_pb";
import type { AgentEntry as PbAgentEntry } from "@roost/shared/proto/wire_pb";
import { agentEntryToProto } from "@roost/shared/wire/agent-proto";
import { AgentEntry } from "@roost/shared/wire/agent-entry";
import { asSessionId } from "@roost/shared/wire";
import { log } from "@roost/shared/log";
import { requireAuth } from "./auth-interceptor.ts";
import { getWorkerHubSocket } from "./worker-service.ts";
import { createPendingRpc } from "../router/pending-rpcs.ts";
import { sendBrowserCmd, forwardToSessionWorker } from "./router-helpers.ts";
import type { ConnectDeps } from "./router.ts";

type AgentSessionMethods =
  | "sessionsAgentRespond" | "sessionsAgentAbort" | "sessionsGetAgentEntries";

// Worker rpc-ok payload → proto AgentEntry[]. The worker replies with Zod-shaped
// AgentEntry JSON, and agentEntryToProto switches on `kind` and then reads body
// fields, so one malformed item would throw mid-map and cost the whole page.
// Validate per entry and skip the bad one instead — the same degrade-don't-crash
// rule sessionRowToProto applies to agent_json.
function _agentEntriesToProto(raw: unknown, sessionId: string): PbAgentEntry[] {
  if (!Array.isArray(raw)) return [];
  const out: PbAgentEntry[] = [];
  for (const item of raw) {
    const parsed = AgentEntry.safeParse(item);
    if (!parsed.success) {
      log.warn("router", "agent_entry_decode_failed", {
        session_id: sessionId, error: parsed.error.issues[0]?.message ?? "invalid",
      });
      continue;
    }
    out.push(agentEntryToProto(parsed.data));
  }
  return out;
}

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

    // Paged transcript backfill out of the worker's in-memory entry ring — the
    // agent counterpart of sessionsGetScrollbackCells, and the ONLY history
    // path: globalAgentEntryBus is volatile, so a reconnecting client rebuilds
    // its transcript from here and lets live frames upsert on top by `seq`.
    async sessionsGetAgentEntries(req, ctx) {
      const caller = requireAuth(ctx.values);
      const row = await deps.db.selectFrom("sessions").select(["worker_fp"]).where("id", "=", req.sessionId).executeTakeFirst();
      if (!row) throw new ConnectError("unknown session", Code.NotFound);
      const sock = getWorkerHubSocket(row.worker_fp);
      if (!sock) throw new ConnectError("worker not connected", Code.Unavailable);
      // The worker's rpc-ok payload is plain JSON, never schema-validated on the
      // way in — so treat every field as possibly-absent below rather than
      // letting a partial reply throw a TypeError the client sees as an opaque
      // Internal error.
      const pending = createPendingRpc<{ entries?: unknown; first_seq?: number; more?: boolean }>(8_000, row.worker_fp);
      sendBrowserCmd(sock, caller, pending.request_id, {
        kind: "get-omp-transcript-page" as const,
        request_id: pending.request_id,
        session_id: asSessionId(req.sessionId),
        // before_seq is uint64 on the wire but the control frame's cursor is a
        // string; "0" means "newest page" (the ring's tail).
        cursor: String(req.beforeSeq),
        limit: 128,
      });
      let res;
      try { res = await pending.promise; }
      catch {
        // THROW on the 8s pending-rpc timeout, same contract as
        // sessionsGetScrollbackCells: an empty page is indistinguishable from
        // "no history" and would leave a permanent hole in the transcript.
        throw new ConnectError("agent entries serve timed out", Code.Unavailable);
      }
      return create(SessionsGetAgentEntriesResponseSchema, {
        entries: _agentEntriesToProto(res.entries, req.sessionId),
        firstSeq: BigInt(res.first_seq ?? 0),
        more: res.more === true,
      });
    },
  };
}
