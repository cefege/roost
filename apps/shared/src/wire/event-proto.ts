// T1.2 — adapters between the in-app Zod SessionEvent and the
// proto SessionEventProto. All 8 variants are proto-typed; the
// JsonEvent fallback path is retired. AgentStatePatch carries the
// sparse-patch wire (with tri-state for current_tool).

import { create } from "@bufbuild/protobuf";
import {
  SessionEventProtoSchema,
  OpenedEvtSchema, ClosedEvtSchema,
  AttachedEvtSchema, DetachedEvtSchema,
  CwdEvtSchema, WorkspaceAssignedEvtSchema,
  AgentEvtSchema, SnapshotEvtSchema,
  RespawnedEvtSchema,
  RenamedEvtSchema,
  GitEvtSchema,
  PrEvtSchema,
  PortsEvtSchema,
  AgentStatePatchSchema,
  AgentToolOpSchema, LastMessageOpSchema,
  CurrentBlockOpSchema, PermissionRequestOpSchema,
  type SessionEventProto, type AgentStatePatch,
} from "../gen/roost/v1/events_pb.ts";
import { asSessionId, asWorkerFp, asWorkspaceId, asChannelId } from "./brand.ts";
import type { AgentState } from "./session.ts";
import {
  tokensToProto, tokensFromProto,
  lastMessageToProto, lastMessageFromProto,
  currentToolToProto, currentToolFromProto,
  currentBlockToProto, currentBlockFromProto,
  permissionRequestToProto, permissionRequestFromProto,
  subAgentRowToProto, subAgentRowFromProto,
  sessionToProto, sessionFromProto,
} from "./agent-proto.ts";
import type { SessionEvent } from "./event.ts";

// Tri-state Op encoder. Centralizes the {undefined→absent, null→clear,
// value→set} ladder so adding a 5th clearable AgentState field can't
// drift on the clear/set branch wiring. <TVal> is the in-app Zod type
// for the field; <TOp> is the generated *Op message proto. Returns
// undefined when value is undefined so caller `p.x = tristateToProto(...)`
// leaves the field absent.
function tristateToProto<TVal>(
  schema: any,
  value: TVal | null | undefined,
  toProto: (v: TVal) => unknown,
): unknown | undefined {
  if (value === undefined) return undefined;
  return create(schema, value === null
    ? { op: { case: "clear", value: true } }
    : { op: { case: "set", value: toProto(value) } });
}

function tristateFromProto<TProtoVal, TVal>(
  op: { op: { case: "set"; value: TProtoVal } | { case: "clear"; value: boolean } | { case: undefined } } | undefined,
  fromProto: (v: TProtoVal) => TVal,
): TVal | null | undefined {
  if (!op) return undefined;
  if (op.op.case === "set") return fromProto(op.op.value);
  if (op.op.case === "clear") return null;
  return undefined;
}

// Sparse Zod AgentState patch → proto AgentStatePatch.
// All four nullable fields use the tristate Op encoder above:
//   undefined → absent (leave alone)
//   null      → { clear: true }
//   value     → { set: <typedProto> }
function agentPatchToProto(patch: Partial<AgentState>): AgentStatePatch {
  const p = create(AgentStatePatchSchema, {});
  if (patch.mode !== undefined) p.mode = patch.mode;
  if (patch.model !== undefined) p.model = patch.model;
  if (patch.status !== undefined) p.status = patch.status;
  if (patch.tokens !== undefined) p.tokens = tokensToProto(patch.tokens);
  if (patch.cost_usd !== undefined) p.costUsd = patch.cost_usd;
  const lm = tristateToProto(LastMessageOpSchema, patch.last_message, lastMessageToProto);
  if (lm !== undefined) p.lastMessage = lm as never;
  const ct = tristateToProto(AgentToolOpSchema, patch.current_tool, currentToolToProto);
  if (ct !== undefined) p.currentTool = ct as never;
  const cb = tristateToProto(CurrentBlockOpSchema, patch.current_block, currentBlockToProto);
  if (cb !== undefined) p.currentBlock = cb as never;
  const pr = tristateToProto(PermissionRequestOpSchema, patch.permission_request, permissionRequestToProto);
  if (pr !== undefined) p.permissionRequest = pr as never;
  if (patch.sub_agents !== undefined) {
    p.subAgents = patch.sub_agents.map(subAgentRowToProto);
    p.hasSubAgents = true;
  }
  return p;
}

// Proto AgentStatePatch → Zod Partial<AgentState>. Does NOT synthesize
// `kind`; the projector re-asserts `kind: "claude"` on every fold
// (foldEvent in event.ts) so the field is redundant on the wire.
function agentProtoToPatch(p: AgentStatePatch): Partial<AgentState> {
  const out: Partial<AgentState> = {};
  if (p.mode !== undefined) out.mode = p.mode as AgentState["mode"];
  if (p.model !== undefined) out.model = p.model;
  if (p.status !== undefined) out.status = p.status as AgentState["status"];
  if (p.tokens) out.tokens = tokensFromProto(p.tokens);
  if (p.costUsd !== undefined) out.cost_usd = p.costUsd;
  const lm = tristateFromProto(p.lastMessage as never, lastMessageFromProto);
  if (lm !== undefined) out.last_message = lm;
  const ct = tristateFromProto(p.currentTool as never, currentToolFromProto);
  if (ct !== undefined) out.current_tool = ct;
  const cb = tristateFromProto(p.currentBlock as never, currentBlockFromProto);
  if (cb !== undefined) out.current_block = cb;
  const pr = tristateFromProto(p.permissionRequest as never, permissionRequestFromProto);
  if (pr !== undefined) out.permission_request = pr;
  if (p.hasSubAgents) out.sub_agents = p.subAgents.map(subAgentRowFromProto);
  return out;
}

/** Encode a SessionEvent as its proto oneof. Exhaustive over every kind —
 * new variants extend the proto + Zod schema + this switch in one pass. */
export function eventToProto(event: SessionEvent, eventId: number): SessionEventProto {
  const eid = BigInt(eventId);
  switch (event.kind) {
    case "opened":
      return create(SessionEventProtoSchema, {
        eventId: eid,
        kind: { case: "opened", value: create(OpenedEvtSchema, {
          sessionId: event.session_id,
          workerFp: event.worker_fp,
          channel: event.channel,
          sessionKind: event.session_kind,
          cwd: event.cwd,
          ts: BigInt(event.ts),
        })},
      });
    case "closed":
      return create(SessionEventProtoSchema, {
        eventId: eid,
        kind: { case: "closed", value: create(ClosedEvtSchema, {
          sessionId: event.session_id,
          exitCode: event.exit_code ?? undefined,
          ts: BigInt(event.ts),
        })},
      });
    case "attached":
      return create(SessionEventProtoSchema, {
        eventId: eid,
        kind: { case: "attached", value: create(AttachedEvtSchema, {
          sessionId: event.session_id, ts: BigInt(event.ts),
        })},
      });
    case "detached":
      return create(SessionEventProtoSchema, {
        eventId: eid,
        kind: { case: "detached", value: create(DetachedEvtSchema, {
          sessionId: event.session_id, ts: BigInt(event.ts),
        })},
      });
    case "cwd":
      return create(SessionEventProtoSchema, {
        eventId: eid,
        kind: { case: "cwd", value: create(CwdEvtSchema, {
          sessionId: event.session_id, cwd: event.cwd, ts: BigInt(event.ts),
        })},
      });
    case "workspace_assigned":
      return create(SessionEventProtoSchema, {
        eventId: eid,
        kind: { case: "workspaceAssigned", value: create(WorkspaceAssignedEvtSchema, {
          sessionId: event.session_id,
          workspaceId: event.workspace_id ?? undefined,
          ts: BigInt(event.ts),
        })},
      });
    case "agent":
      return create(SessionEventProtoSchema, {
        eventId: eid,
        kind: { case: "agent", value: create(AgentEvtSchema, {
          sessionId: event.session_id,
          patch: agentPatchToProto(event.patch),
          ts: BigInt(event.ts),
        })},
      });
    case "snapshot":
      return create(SessionEventProtoSchema, {
        eventId: eid,
        kind: { case: "snapshot", value: create(SnapshotEvtSchema, {
          workerFp: event.worker_fp,
          sessions: event.sessions.map(sessionToProto),
          ts: BigInt(event.ts),
        })},
      });
    case "respawned":
      return create(SessionEventProtoSchema, {
        eventId: eid,
        kind: { case: "respawned", value: create(RespawnedEvtSchema, {
          sessionId: event.session_id,
          newChannel: event.new_channel,
          ts: BigInt(event.ts),
        })},
      });
    case "renamed":
      return create(SessionEventProtoSchema, {
        eventId: eid,
        kind: { case: "renamed", value: create(RenamedEvtSchema, {
          sessionId: event.session_id,
          customTitle: event.custom_title,
          ts: BigInt(event.ts),
        })},
      });
    case "git":
      return create(SessionEventProtoSchema, {
        eventId: eid,
        kind: { case: "git", value: create(GitEvtSchema, {
          sessionId: event.session_id,
          branch: event.branch ?? undefined,
          remote: event.remote ?? undefined,
          ts: BigInt(event.ts),
        })},
      });
    case "pr":
      return create(SessionEventProtoSchema, {
        eventId: eid,
        kind: { case: "pr", value: create(PrEvtSchema, {
          sessionId: event.session_id,
          number: event.number ?? undefined,
          state: event.state ?? undefined,
          checks: event.checks ?? undefined,
          url: event.url ?? undefined,
          ts: BigInt(event.ts),
        })},
      });
    case "ports":
      return create(SessionEventProtoSchema, {
        eventId: eid,
        kind: { case: "ports", value: create(PortsEvtSchema, {
          sessionId: event.session_id,
          ports: event.ports,
          ts: BigInt(event.ts),
        })},
      });
  }
}

export function protoToEvent(p: SessionEventProto): (SessionEvent & { _event_id: number }) | null {
  const eid = Number(p.eventId);
  const k = p.kind?.case;
  if (!k) return null;
  const v = p.kind.value as any;
  switch (k) {
    case "opened":
      return {
        kind: "opened",
        session_id: asSessionId(v.sessionId),
        worker_fp: asWorkerFp(v.workerFp),
        channel: asChannelId(v.channel),
        session_kind: v.sessionKind,
        cwd: v.cwd,
        ts: Number(v.ts),
        _event_id: eid,
      } as never;
    case "closed":
      return {
        kind: "closed",
        session_id: asSessionId(v.sessionId),
        exit_code: v.exitCode ?? null,
        ts: Number(v.ts),
        _event_id: eid,
      } as never;
    case "attached":
      return {
        kind: "attached",
        session_id: asSessionId(v.sessionId),
        ts: Number(v.ts),
        _event_id: eid,
      } as never;
    case "detached":
      return {
        kind: "detached",
        session_id: asSessionId(v.sessionId),
        ts: Number(v.ts),
        _event_id: eid,
      } as never;
    case "cwd":
      return {
        kind: "cwd",
        session_id: asSessionId(v.sessionId),
        cwd: v.cwd,
        ts: Number(v.ts),
        _event_id: eid,
      } as never;
    case "workspaceAssigned":
      return {
        kind: "workspace_assigned",
        session_id: asSessionId(v.sessionId),
        workspace_id: v.workspaceId ? asWorkspaceId(v.workspaceId) : null,
        ts: Number(v.ts),
        _event_id: eid,
      } as never;
    case "agent": {
      return {
        kind: "agent",
        session_id: asSessionId(v.sessionId),
        patch: v.patch ? agentProtoToPatch(v.patch) : {},
        ts: Number(v.ts),
        _event_id: eid,
      } as never;
    }
    case "snapshot": {
      return {
        kind: "snapshot",
        worker_fp: asWorkerFp(v.workerFp),
        sessions: v.sessions.map(sessionFromProto),
        ts: Number(v.ts),
        _event_id: eid,
      } as never;
    }
    case "respawned":
      return {
        kind: "respawned",
        session_id: asSessionId(v.sessionId),
        new_channel: asChannelId(v.newChannel),
        ts: Number(v.ts),
        _event_id: eid,
      } as never;
    case "renamed":
      return {
        kind: "renamed",
        session_id: asSessionId(v.sessionId),
        custom_title: v.customTitle ?? "",
        ts: Number(v.ts),
        _event_id: eid,
      } as never;
    case "git":
      return {
        kind: "git",
        session_id: asSessionId(v.sessionId),
        branch: v.branch ?? null,
        // Only carry remote when the wire had it (absent → preserve prior).
        ...(v.remote ? { remote: v.remote } : {}),
        ts: Number(v.ts),
        _event_id: eid,
      } as never;
    case "pr":
      return {
        kind: "pr",
        session_id: asSessionId(v.sessionId),
        number: v.number ?? null,
        state: v.state ?? null,
        checks: v.checks ?? null,
        url: v.url ?? null,
        ts: Number(v.ts),
        _event_id: eid,
      } as never;
    case "ports":
      return {
        kind: "ports",
        session_id: asSessionId(v.sessionId),
        ports: v.ports ?? [],
        ts: Number(v.ts),
        _event_id: eid,
      } as never;
    default:
      return null;
  }
}
