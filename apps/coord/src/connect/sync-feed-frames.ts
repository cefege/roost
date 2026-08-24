// Stateless bus-payload → FirehoseFrame adapters for the Sync firehose, plus the
// frame → lane/domain metadata the socket scheduler reads. Split out of
// handlers-streaming.ts; the engine that installs these on the live buses and
// seeds them is sync-feed.ts.

import { create } from "@bufbuild/protobuf";
import { log } from "@roost/shared/log";
import {
  FirehoseFrameSchema, type FirehoseFrame,
  AgentStatusFrameSchema, SyncDomain,
} from "@roost/shared/proto/sync_pb";
import { eventToProto } from "@roost/shared/wire/event-proto";
import {
  WorkspaceDeltaProtoSchema, WorkspaceSessionsSetSchema,
  TaskDeltaProtoSchema, WebhookTokenDeltaProtoSchema,
  PermissionRuleDeltaProtoSchema, McpStreamMessageProtoSchema,
  McpRelayEventSchema, WorkerPresenceProtoSchema, WorkerHeartbeatSchema,
  PairRequestDeltaProtoSchema,
} from "@roost/shared/proto/events_pb";
import {
  WorkspaceSchema as WorkspacePbSchema,
  WebhookTokenSchema as WebhookTokenPbSchema,
  PermissionRuleSchema as PermissionRulePbSchema,
  McpRelaySchema as McpRelayPbSchema,
  WorkerSchema as WorkerPbSchema,
  HostMetricsSchema as HostMetricsPbSchema,
  AuditRowSchema, PairRequestSchema,
} from "@roost/shared/proto/wire_pb";
import type { TaskBusMsg, PairRequestDelta, AuditRow } from "../buses.ts";
import type {
  SessionEvent, WorkspaceDelta, WebhookTokenDelta, PermissionRuleDelta,
  McpStreamMessage, WorkerPresenceEvent, HostMetrics, AgentStatusUpdate,
} from "@roost/shared/wire";

export type SyncFeedLane = "cell" | "session" | "retained" | "nonterminal" | "control";

export interface SyncFeedFrameMeta {
  readonly domain: SyncDomain | null;
  readonly lane: SyncFeedLane;
  readonly sessionId?: string;
  readonly announces?: readonly string[];
  readonly closes?: readonly string[];
  /** Retained snapshot item that must precede the pre-ready live segment. */
  readonly beforeBuffered?: boolean;
  /** Scheduler-owned incremental terminal snapshot cursor metadata. */
  readonly terminalStreamId?: string;
  readonly terminalCursorIndex?: number;
  /** Baseline frame of a freshly attached session; the v2 egress scheduler may
   *  jump other sessions' queued deltas but never their snapshots or this
   *  session's own queued frames. */
  readonly attachSnapshot?: boolean;
}

export function frameMeta(frame: FirehoseFrame): SyncFeedFrameMeta {
  switch (frame.frame.case) {
    case "cellGrid":
      return { domain: SyncDomain.TERMINAL, lane: "cell", sessionId: frame.frame.value.sessionId };
    case "cellGridChunk":
      return {
        domain: SyncDomain.TERMINAL,
        lane: "cell",
        sessionId: frame.frame.value.part?.sessionId,
      };
    case "terminalViewState":
      return {
        domain: SyncDomain.TERMINAL,
        lane: "cell",
        sessionId: frame.frame.value.sessionId,
      };
    case "sessions": {
      try {
        const event = JSON.parse(frame.frame.value.payloadJson) as {
          kind?: unknown;
          session_id?: unknown;
        };
        const sessionId = typeof event.session_id === "string" ? event.session_id : undefined;
        return {
          domain: SyncDomain.TERMINAL,
          lane: "session",
          sessionId,
          announces: event.kind === "opened" && sessionId ? [sessionId] : undefined,
          closes: event.kind === "closed" && sessionId ? [sessionId] : undefined,
        };
      } catch (error) {
        // The JsonEvent fallback still delivers the payload — only the
        // routing metadata is lost, so this stays at debug level.
        log.debug("connect.sync", "session_meta_parse_failed", {
          dropped_fields: ["session_id", "announces", "closes"],
          error: String(error),
        });
        return { domain: SyncDomain.TERMINAL, lane: "session" };
      }
    }
    case "sessionEvent": {
      const kind = frame.frame.value.kind;
      if (kind.case === "opened") {
        return {
          domain: SyncDomain.TERMINAL,
          lane: "session",
          sessionId: kind.value.sessionId,
          announces: [kind.value.sessionId],
        };
      }
      if (kind.case === "closed") {
        return {
          domain: SyncDomain.TERMINAL,
          lane: "session",
          sessionId: kind.value.sessionId,
          closes: [kind.value.sessionId],
        };
      }
      if (kind.case === "snapshot") {
        return {
          domain: SyncDomain.TERMINAL,
          lane: "session",
          announces: kind.value.sessions.map((session) => session.id),
        };
      }
      const sessionId = kind.case === undefined ? undefined : "sessionId" in kind.value
        ? kind.value.sessionId
        : undefined;
      return { domain: SyncDomain.TERMINAL, lane: "session", sessionId };
    }
    case "sessionPresence":
    case "terminalTitle":
    case "lastActivity":
    case "agentStatus":
      return {
        domain: SyncDomain.TERMINAL,
        lane: "session",
        sessionId: frame.frame.value.sessionId,
      };
    case "workerPresence":
    case "workerRoutable":
      return { domain: SyncDomain.WORKERS, lane: "nonterminal" };
    case "workspaceDelta":
      return { domain: SyncDomain.WORKSPACES, lane: "nonterminal" };
    case "taskDelta":
      return { domain: SyncDomain.TASKS, lane: "nonterminal" };
    case "permissionDelta":
      return { domain: SyncDomain.PERMISSIONS, lane: "nonterminal" };
    case "mcpMsg":
      return { domain: SyncDomain.MCP, lane: "nonterminal" };
    case "pairRequestDelta":
      return { domain: SyncDomain.PAIR, lane: "nonterminal" };
    case "webhookTokenDelta":
      return { domain: SyncDomain.WEBHOOK, lane: "nonterminal" };
    case "auditRow":
      return { domain: SyncDomain.AUDIT, lane: "nonterminal" };
    case "uiState":
    case "uiCommand":
    case "keepalive":
    case "coordinatorRelocation":
    case "subscribed":
    case "domainReset":
    case "inputAccepted":
    case "inputRejected":
    case "inputAmbiguous":
    case undefined:
      return { domain: null, lane: "control" };
  }
}

// Backfill + live sessionBus both encode SessionEvent through here.
export const sessionFirehoseFrame = (e: SessionEvent, eventId: number): FirehoseFrame =>
  create(FirehoseFrameSchema, { frame: { case: "sessionEvent", value: eventToProto(e, eventId) } });

// T1.2 part 2 — typed delta adapters. Each returns a proto-shaped
// FirehoseFrame when the bus payload matches a known shape, else
// null so the caller falls back to the JsonEvent path.
export const workspaceFrame = (e: WorkspaceDelta): FirehoseFrame | null => {
  if (e.kind === "created" || e.kind === "updated") {
    return create(FirehoseFrameSchema, { frame: { case: "workspaceDelta", value: create(WorkspaceDeltaProtoSchema, {
      kind: { case: e.kind, value: create(WorkspacePbSchema, {
        id: e.workspace.id, workerFp: e.workspace.worker_fp,
        name: e.workspace.name, folderPath: e.workspace.folder_path,
        color: e.workspace.color ?? undefined, position: e.workspace.position,
        version: BigInt(e.workspace.version),
        createdAtMs: BigInt(e.workspace.created_at_ms),
        updatedAtMs: BigInt(e.workspace.updated_at_ms),
        sessionIds: e.workspace.session_ids,
      }) },
    })}});
  }
  if (e.kind === "deleted") {
    return create(FirehoseFrameSchema, { frame: { case: "workspaceDelta", value: create(WorkspaceDeltaProtoSchema, {
      kind: { case: "deletedId", value: e.id },
    })}});
  }
  if (e.kind === "sessions-set") {
    return create(FirehoseFrameSchema, { frame: { case: "workspaceDelta", value: create(WorkspaceDeltaProtoSchema, {
      kind: { case: "sessionsSet", value: create(WorkspaceSessionsSetSchema, {
        workspaceId: e.id, sessionIds: e.session_ids, version: BigInt(e.version),
      }) },
    })}});
  }
  return null;
};

export const taskFrame = (e: TaskBusMsg): FirehoseFrame =>
  create(FirehoseFrameSchema, { frame: { case: "taskDelta", value: create(TaskDeltaProtoSchema, {
    kind: { case: e.kind, value: e.task },
  })}});

export const webhookFrame = (e: WebhookTokenDelta): FirehoseFrame | null => {
  if (e.kind === "created") {
    return create(FirehoseFrameSchema, { frame: { case: "webhookTokenDelta", value: create(WebhookTokenDeltaProtoSchema, {
      kind: { case: "created", value: create(WebhookTokenPbSchema, {
        id: e.token.id, label: e.token.label, last4: e.token.last4, scopes: e.token.scopes,
        createdAtMs: BigInt(e.token.created_at_ms),
        lastUsedAtMs: e.token.last_used_at_ms != null ? BigInt(e.token.last_used_at_ms) : undefined,
      }) },
    })}});
  }
  if (e.kind === "deleted") {
    return create(FirehoseFrameSchema, { frame: { case: "webhookTokenDelta", value: create(WebhookTokenDeltaProtoSchema, {
      kind: { case: "deletedId", value: e.id },
    })}});
  }
  return null;
};

export const permFrame = (e: PermissionRuleDelta): FirehoseFrame | null => {
  if (e.kind === "created" || e.kind === "updated") {
    return create(FirehoseFrameSchema, { frame: { case: "permissionDelta", value: create(PermissionRuleDeltaProtoSchema, {
      kind: { case: e.kind, value: create(PermissionRulePbSchema, {
        id: e.rule.id, toolPattern: e.rule.tool_pattern, folderGlob: e.rule.folder_glob,
        decision: e.rule.decision, enabled: e.rule.enabled,
        createdAtMs: BigInt(e.rule.created_at_ms),
      }) },
    })}});
  }
  if (e.kind === "deleted") {
    return create(FirehoseFrameSchema, { frame: { case: "permissionDelta", value: create(PermissionRuleDeltaProtoSchema, {
      kind: { case: "deletedId", value: e.id },
    })}});
  }
  return null;
};

export const mcpFrame = (e: McpStreamMessage): FirehoseFrame | null => {
  if ("kind" in e && (e.kind === "created" || e.kind === "updated")) {
    return create(FirehoseFrameSchema, { frame: { case: "mcpMsg", value: create(McpStreamMessageProtoSchema, {
      kind: { case: e.kind, value: create(McpRelayPbSchema, {
        id: e.relay.id, label: e.relay.label, kind: e.relay.kind,
        configJson: JSON.stringify(e.relay.config),
        createdAtMs: BigInt(e.relay.created_at_ms),
      }) },
    })}});
  }
  if ("kind" in e && e.kind === "deleted") {
    return create(FirehoseFrameSchema, { frame: { case: "mcpMsg", value: create(McpStreamMessageProtoSchema, {
      kind: { case: "deletedId", value: e.id },
    })}});
  }
  if ("relay_id" in e && "payload" in e) {
    return create(FirehoseFrameSchema, { frame: { case: "mcpMsg", value: create(McpStreamMessageProtoSchema, {
      kind: { case: "event", value: create(McpRelayEventSchema, {
        relayId: e.relay_id, payloadJson: JSON.stringify(e.payload), ts: BigInt(e.ts),
      }) },
    })}});
  }
  return null;
};

export const presenceFrame = (e: WorkerPresenceEvent): FirehoseFrame | null => {
  const hm = (m: HostMetrics) => create(HostMetricsPbSchema, {
    cpuPct: m.cpu_pct,
    memUsedBytes: BigInt(m.mem_used_bytes), memTotalBytes: BigInt(m.mem_total_bytes),
    diskUsedBytes: BigInt(m.disk_used_bytes), diskTotalBytes: BigInt(m.disk_total_bytes),
    netRxBps: BigInt(m.net_rx_bps), netTxBps: BigInt(m.net_tx_bps),
    sampledAtMs: BigInt(m.sampled_at_ms),
  });
  if (e.kind === "registered") {
    return create(FirehoseFrameSchema, { frame: { case: "workerPresence", value: create(WorkerPresenceProtoSchema, {
      kind: { case: "registered", value: create(WorkerPbSchema, {
        fp: e.worker.fp, label: e.worker.label, os: e.worker.os,
        gitSha: e.worker.git_sha ?? undefined,
        hostMetrics: e.worker.host_metrics ? hm(e.worker.host_metrics) : undefined,
        registeredAtMs: BigInt(e.worker.registered_at_ms),
        lastSeenMs: BigInt(e.worker.last_seen_ms),
      }) },
    })}});
  }
  if (e.kind === "heartbeat") {
    return create(FirehoseFrameSchema, { frame: { case: "workerPresence", value: create(WorkerPresenceProtoSchema, {
      kind: { case: "heartbeat", value: create(WorkerHeartbeatSchema, {
        workerFp: e.fp, lastSeenMs: BigInt(e.last_seen_ms),
        hostMetrics: e.host_metrics ? hm(e.host_metrics) : undefined,
      }) },
    })}});
  }
  if (e.kind === "removed") {
    return create(FirehoseFrameSchema, { frame: { case: "workerPresence", value: create(WorkerPresenceProtoSchema, {
      kind: { case: "removedFp", value: e.fp },
    })}});
  }
  return null;
};

export const auditFrame = (e: AuditRow): FirehoseFrame =>
  create(FirehoseFrameSchema, { frame: { case: "auditRow", value: create(AuditRowSchema, {
    id: BigInt(e.id), ts: BigInt(e.ts),
    callerFp: e.caller_fp ?? undefined,
    callerLabel: e.caller_label ?? undefined,
    method: e.method, path: e.path, status: e.status,
    traceId: e.trace_id ?? undefined,
  })}});

// Pair-request deltas (perf sweep C2.4 — replaces the SPA pairList
// poller). Bus payload is the coord-internal PairRequestDelta shape.
export const pairFrame = (e: PairRequestDelta): FirehoseFrame =>
  e.kind === "pending"
    ? create(FirehoseFrameSchema, { frame: { case: "pairRequestDelta", value: create(PairRequestDeltaProtoSchema, {
        kind: { case: "pending", value: create(PairRequestSchema, {
          ephemeralId: e.ephemeral_id, label: e.label, createdAtMs: BigInt(e.created_at_ms),
        }) },
      })}})
    : create(FirehoseFrameSchema, { frame: { case: "pairRequestDelta", value: create(PairRequestDeltaProtoSchema, {
        kind: { case: "removedId", value: e.ephemeral_id },
      })}});

export const agentStatusFrame = (status: AgentStatusUpdate): FirehoseFrame =>
  create(FirehoseFrameSchema, {
    frame: {
      case: "agentStatus",
      value: create(AgentStatusFrameSchema, {
        sessionId: status.session_id,
        agentId: status.agent_id,
        state: status.state,
        message: status.message,
        revision: BigInt(status.revision),
        completedRevision: BigInt(status.completed_revision),
        updatedAt: status.updated_at,
        active: status.active,
      }),
    },
  });

export const sessionMeta = (event: SessionEvent): SyncFeedFrameMeta => {
  const announces = event.kind === "opened"
    ? [String(event.session_id)]
    : event.kind === "snapshot"
      ? event.sessions.map((session) => String(session.id))
      : undefined;
  const closes = event.kind === "closed" ? [String(event.session_id)] : undefined;
  const sessionId = "session_id" in event ? String(event.session_id) : undefined;
  return {
    domain: SyncDomain.TERMINAL,
    lane: "session",
    sessionId,
    announces,
    closes,
  };
};
