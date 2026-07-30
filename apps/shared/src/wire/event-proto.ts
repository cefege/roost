// Adapters between the in-app Zod SessionEvent and the typed
// proto SessionEventProto oneof.

import { create } from "@bufbuild/protobuf";
import {
  SessionEventProtoSchema,
  OpenedEvtSchema, ClosedEvtSchema,
  AttachedEvtSchema, DetachedEvtSchema,
  CwdEvtSchema, WorkspaceAssignedEvtSchema,
  SnapshotEvtSchema, RespawnedEvtSchema,
  RenamedEvtSchema, GitEvtSchema, PrEvtSchema, PortsEvtSchema,
  type SessionEventProto,
} from "../gen/roost/v1/events_pb.ts";
import { asSessionId, asWorkerFp, asWorkspaceId, asChannelId } from "./brand.ts";
import { sessionToProto, sessionFromProto } from "./session-proto.ts";
import type { SessionEvent } from "./event.ts";


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
