// proto→wire adapters for the typed bus deltas carried on the Sync stream.
// Each converts a generated protobuf delta (oneof `kind`) into the legacy
// snake_case wire shape the _handle* projectors in sync-handlers.ts already
// consume, so adding a typed bus didn't force a projector rewrite. Pure
// functions, no module state. Callers: store/sync-frame.ts (live deltas)
// and store/sync-bootstrap.ts (hydration lists reuse taskProtoToWire /
// mcpRelayProtoToWire so both paths decode identically).
//
// Coord persists task payload/result and MCP relay config as raw *_json
// columns, so those strings can be malformed independent of proto framing.
// decodeWireJson turns such a row into a DROPPED record plus a
// diag.corruption_signal instead of a thrown parse error — _dispatchV2Application
// treats any dispatch throw as fatal protocol damage that tears down the v2
// socket, which one bad DB row must never cause.

import { signal } from "@roost/shared/diag";
import type { McpRelay as McpRelayWire, Task as TaskWire } from "@roost/shared/wire";
import type { HostMetrics, McpRelay, Task, Workspace } from "@roost/shared/proto/wire_pb";
import type {
  McpStreamMessageProto,
  TaskDeltaProto,
  WorkerPresenceProto,
  WorkspaceDeltaProto,
} from "@roost/shared/proto/events_pb";

/**
 * Decode one *_json column riding a proto delta or hydration list. Returns
 * undefined on malformed JSON after signalling; callers drop the record.
 */
export function decodeWireJson<T>(raw: string, frame: string): T | undefined {
  try {
    return JSON.parse(raw) as T;
  } catch (error) {
    signal("diag.corruption_signal", {
      kind: "sync_json_parse",
      frame,
      msg: String(error),
      cooldownKey: "sync",
    });
    return undefined;
  }
}

function hostMetricsProtoToWire(m: HostMetrics | undefined) {
  return m ? {
    cpu_pct: m.cpuPct,
    mem_used_bytes: Number(m.memUsedBytes), mem_total_bytes: Number(m.memTotalBytes),
    disk_used_bytes: Number(m.diskUsedBytes), disk_total_bytes: Number(m.diskTotalBytes),
    net_rx_bps: Number(m.netRxBps), net_tx_bps: Number(m.netTxBps),
    sampled_at_ms: Number(m.sampledAtMs),
  } : null;
}

function workspaceProtoToWire(w: Workspace) {
  return {
    id: w.id, worker_fp: w.workerFp, name: w.name, folder_path: w.folderPath,
    color: w.color ?? null, position: w.position, version: Number(w.version),
    created_at_ms: Number(w.createdAtMs), updated_at_ms: Number(w.updatedAtMs),
    session_ids: w.sessionIds,
  };
}

/** One proto Task → legacy snake_case task wire shape. Null (after a
 *  sync_json_parse signal) when payload/result JSON is malformed — the
 *  caller drops the record rather than folding a half-decoded task. */
export function taskProtoToWire(v: Task): TaskWire | null {
  const payload = decodeWireJson<TaskWire["payload"]>(v.payloadJson, "task");
  if (payload === undefined) return null;
  let result = null as TaskWire["result"];
  if (v.resultJson) {
    const decoded = decodeWireJson<NonNullable<TaskWire["result"]>>(v.resultJson, "task");
    if (decoded === undefined) return null;
    result = decoded;
  }
  return {
    // Proto ids/states are plain strings; the zod brands are enforced at the
    // store boundary (Task.safeParse consumers), not duplicated here.
    id: v.id as TaskWire["id"],
    state: v.state as TaskWire["state"],
    payload,
    enqueued_at_ms: Number(v.enqueuedAtMs),
    claimed_at_ms: v.claimedAtMs != null ? Number(v.claimedAtMs) : null,
    claimed_by: (v.claimedBy ?? null) as TaskWire["claimed_by"],
    finished_at_ms: v.finishedAtMs != null ? Number(v.finishedAtMs) : null,
    result,
    completion_check: v.completionCheck ?? null,
    completion_check_last_attempt_ms: v.completionCheckLastAttemptMs != null
      ? Number(v.completionCheckLastAttemptMs)
      : null,
    claim_ttl_ms: Number(v.claimTtlMs),
  };
}

export function _workspaceProtoToWire(d: WorkspaceDeltaProto) {
  switch (d.kind.case) {
    case "created":   return { kind: "created", workspace: workspaceProtoToWire(d.kind.value) };
    case "updated":   return { kind: "updated", workspace: workspaceProtoToWire(d.kind.value) };
    case "deletedId": return { kind: "deleted", id: d.kind.value };
    case "sessionsSet": return {
      kind: "sessions-set",
      id: d.kind.value.workspaceId,
      session_ids: d.kind.value.sessionIds,
      version: Number(d.kind.value.version),
    };
  }
  return null;
}

export function _taskProtoToWire(d: TaskDeltaProto) {
  const v = d.kind.value;
  if (!v) return null;
  const taskWire = taskProtoToWire(v);
  if (!taskWire) return null;
  switch (d.kind.case) {
    case "created": return { kind: "created", task: taskWire };
    case "state":   return { kind: "state",   task: taskWire };
  }
  return null;
}

/** One proto McpRelay → legacy wire shape. Null (after a sync_json_parse
 *  signal) when config_json is malformed. */
export function mcpRelayProtoToWire(v: McpRelay): McpRelayWire | null {
  const config = decodeWireJson<McpRelayWire["config"]>(v.configJson, "mcp");
  if (config === undefined) return null;
  return {
    // See taskProtoToWire on the plain-string → brand casts.
    id: v.id as McpRelayWire["id"],
    label: v.label,
    kind: v.kind as McpRelayWire["kind"],
    config,
    created_at_ms: Number(v.createdAtMs),
  };
}

export function _mcpProtoToWire(d: McpStreamMessageProto) {
  switch (d.kind.case) {
    case "created":
    case "updated": {
      const relay = mcpRelayProtoToWire(d.kind.value);
      if (!relay) return null;
      return { kind: d.kind.case, relay };
    }
    case "deletedId": return { kind: "deleted", id: d.kind.value };
    case "event": {
      const v = d.kind.value;
      const payload = decodeWireJson<unknown>(v.payloadJson, "mcpEvent");
      if (payload === undefined) return null;
      return { relay_id: v.relayId, payload, ts: Number(v.ts) };
    }
  }
  return null;
}

export function _presenceProtoToWire(d: WorkerPresenceProto) {
  switch (d.kind.case) {
    case "registered": {
      const v = d.kind.value;
      return {
        kind: "registered",
        worker: {
          fp: v.fp, label: v.label, os: v.os, git_sha: v.gitSha ?? null,
          host_metrics: hostMetricsProtoToWire(v.hostMetrics),
          registered_at_ms: Number(v.registeredAtMs),
          last_seen_ms: Number(v.lastSeenMs),
          reachable_addr: v.reachableAddr ?? null,
          keeper_stale: v.keeperStale ?? null,
        },
      };
    }
    case "heartbeat": {
      const v = d.kind.value;
      return {
        kind: "heartbeat", fp: v.workerFp, last_seen_ms: Number(v.lastSeenMs),
        host_metrics: hostMetricsProtoToWire(v.hostMetrics),
      };
    }
    case "removedFp": return { kind: "removed", fp: d.kind.value };
  }
  return null;
}
