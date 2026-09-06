// Maps the in-app SessionEvent union to and from its protobuf oneof.
// Worker transport and browser Sync both depend on this exhaustive adapter.
// Decoded events are rebuilt through the canonical bounded Zod schema.

import { create } from "@bufbuild/protobuf";
import {
  SessionEventProtoSchema,
  OpenedEvtSchema, ClosedEvtSchema,
  AttachedEvtSchema, DetachedEvtSchema,
  CwdEvtSchema, WorkspaceAssignedEvtSchema,
  SnapshotEvtSchema, RespawnedEvtSchema,
  RenamedEvtSchema, GitEvtSchema, PrEvtSchema, PortsEvtSchema,
  AgentReferenceEvtSchema, type SessionEventProto,
} from "../gen/roost/v1/events_pb.ts";
import { asSessionId, asWorkerFp, asWorkspaceId, asChannelId } from "./brand.ts";
import { sessionToProto, sessionFromProto } from "./session-proto.ts";
import {
  agentConversationReferenceFromProto,
  agentConversationReferenceToProto,
} from "../agent-conversation-reference-proto.ts";
import { SessionEvent } from "./event.ts";


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
    case "agent_reference": {
      const checked = SessionEvent.parse(event);
      if (checked.kind !== "agent_reference") {
        throw new Error("invalid agent conversation reference event");
      }
      return create(SessionEventProtoSchema, {
        eventId: eid,
        kind: { case: "agentReference", value: create(AgentReferenceEvtSchema, {
          sessionId: checked.session_id,
          reference: checked.reference
            ? agentConversationReferenceToProto(checked.reference)
            : undefined,
          ts: BigInt(checked.ts),
          traceId: checked.trace_id ?? undefined,
        })},
      });
    }
  }
}

export function protoToEvent(
  eventProto: SessionEventProto,
): (SessionEvent & { _event_id: number }) | null {
  const eventId = Number(eventProto.eventId);
  const kind = eventProto.kind;
  switch (kind.case) {
    case "opened":
      return decodedEvent(eventId, {
        kind: "opened",
        session_id: asSessionId(kind.value.sessionId),
        worker_fp: asWorkerFp(kind.value.workerFp),
        channel: asChannelId(kind.value.channel),
        session_kind: kind.value.sessionKind,
        cwd: kind.value.cwd,
        ts: Number(kind.value.ts),
      });
    case "closed":
      return decodedEvent(eventId, {
        kind: "closed",
        session_id: asSessionId(kind.value.sessionId),
        exit_code: kind.value.exitCode ?? null,
        ts: Number(kind.value.ts),
      });
    case "attached":
      return decodedEvent(eventId, {
        kind: "attached",
        session_id: asSessionId(kind.value.sessionId),
        ts: Number(kind.value.ts),
      });
    case "detached":
      return decodedEvent(eventId, {
        kind: "detached",
        session_id: asSessionId(kind.value.sessionId),
        ts: Number(kind.value.ts),
      });
    case "cwd":
      return decodedEvent(eventId, {
        kind: "cwd",
        session_id: asSessionId(kind.value.sessionId),
        cwd: kind.value.cwd,
        ts: Number(kind.value.ts),
      });
    case "workspaceAssigned":
      return decodedEvent(eventId, {
        kind: "workspace_assigned",
        session_id: asSessionId(kind.value.sessionId),
        workspace_id: kind.value.workspaceId
          ? asWorkspaceId(kind.value.workspaceId)
          : null,
        ts: Number(kind.value.ts),
      });
    case "snapshot":
      return decodedEvent(eventId, {
        kind: "snapshot",
        worker_fp: asWorkerFp(kind.value.workerFp),
        sessions: kind.value.sessions.map(sessionFromProto),
        ts: Number(kind.value.ts),
      });
    case "respawned":
      return decodedEvent(eventId, {
        kind: "respawned",
        session_id: asSessionId(kind.value.sessionId),
        new_channel: asChannelId(kind.value.newChannel),
        ts: Number(kind.value.ts),
      });
    case "renamed":
      return decodedEvent(eventId, {
        kind: "renamed",
        session_id: asSessionId(kind.value.sessionId),
        custom_title: kind.value.customTitle ?? "",
        ts: Number(kind.value.ts),
      });
    case "git":
      return decodedEvent(eventId, {
        kind: "git",
        session_id: asSessionId(kind.value.sessionId),
        branch: kind.value.branch ?? null,
        ...(kind.value.remote !== undefined
          ? { remote: kind.value.remote }
          : {}),
        ts: Number(kind.value.ts),
      });
    case "pr":
      return decodedEvent(eventId, {
        kind: "pr",
        session_id: asSessionId(kind.value.sessionId),
        number: kind.value.number ?? null,
        state: kind.value.state ?? null,
        checks: kind.value.checks ?? null,
        url: kind.value.url ?? null,
        ts: Number(kind.value.ts),
      });
    case "ports":
      return decodedEvent(eventId, {
        kind: "ports",
        session_id: asSessionId(kind.value.sessionId),
        ports: kind.value.ports,
        ts: Number(kind.value.ts),
      });
    case "agentReference":
      return decodedEvent(eventId, {
        kind: "agent_reference",
        session_id: asSessionId(kind.value.sessionId),
        reference: kind.value.reference
          ? agentConversationReferenceFromProto(kind.value.reference)
          : null,
        ...(kind.value.traceId !== undefined
          ? { trace_id: kind.value.traceId }
          : {}),
        ts: Number(kind.value.ts),
      });
    case undefined:
      return null;
  }
}

function decodedEvent(
  eventId: number,
  value: unknown,
): SessionEvent & { _event_id: number } {
  return { ...SessionEvent.parse(value), _event_id: eventId };
}
