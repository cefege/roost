// Pure DB-row → protobuf adapters for the Session-adjacent entities. Session
// itself uses session-proto.ts; adding a field to a sibling entity requires
// editing only this module.
//
// workspaceRowToProto stays in coord/router.ts because it's async
// (joins workspace_sessions). Everything else here is a pure function.

import { create } from "@bufbuild/protobuf";
import {
  WorkerSchema, HostMetricsSchema,
  TaskSchema, McpRelaySchema,
  type Worker as PbWorker,
  type Task as PbTask,
  type McpRelay as PbMcpRelay,
} from "../gen/roost/v1/wire_pb.ts";
import { safeJsonParse } from "../json.ts";

// Wire-shape (Zod) Worker payload for presenceBus.publish. Used by
// workersRegister / workersHeartbeat / workersRename — three near-
// identical inline blocks before this helper existed.
export interface WireWorkerPresence {
  fp: string; label: string; os: string;
  git_sha: string | null;
  host_metrics: unknown;
  registered_at_ms: number; last_seen_ms: number;
  reachable_addr: string | null;
  keeper_stale: string | null;
}
export function workerRowToWirePresence(row: {
  fp: string; label: string; os: string; git_sha: string | null;
  host_metrics_json: string | null;
  registered_at_ms: number; last_seen_ms: number;
  reachable_addr: string | null;
  keeper_stale: string | null;
}): WireWorkerPresence {
  return {
    fp: row.fp, label: row.label, os: row.os,
    git_sha: row.git_sha ?? null,
    host_metrics: safeJsonParse(row.host_metrics_json, null, "host_metrics_json"),
    registered_at_ms: row.registered_at_ms,
    last_seen_ms: row.last_seen_ms,
    reachable_addr: row.reachable_addr ?? null,
    keeper_stale: row.keeper_stale,
  };
}

export function workerRowToProto(row: {
  fp: string; label: string; os: string; git_sha: string | null;
  host_metrics_json: string | null;
  registered_at_ms: number; last_seen_ms: number;
  reachable_addr: string | null;
  keeper_stale: string | null;
}): PbWorker {
  const hostMetricsRaw: any = safeJsonParse(row.host_metrics_json, null, "host_metrics_json");
  return create(WorkerSchema, {
    fp: row.fp,
    label: row.label,
    os: row.os,
    gitSha: row.git_sha ?? undefined,
    hostMetrics: hostMetricsRaw ? create(HostMetricsSchema, {
      cpuPct: hostMetricsRaw.cpu_pct,
      memUsedBytes: BigInt(hostMetricsRaw.mem_used_bytes ?? 0),
      memTotalBytes: BigInt(hostMetricsRaw.mem_total_bytes ?? 0),
      diskUsedBytes: BigInt(hostMetricsRaw.disk_used_bytes ?? 0),
      diskTotalBytes: BigInt(hostMetricsRaw.disk_total_bytes ?? 0),
      netRxBps: BigInt(hostMetricsRaw.net_rx_bps ?? 0),
      netTxBps: BigInt(hostMetricsRaw.net_tx_bps ?? 0),
      sampledAtMs: BigInt(hostMetricsRaw.sampled_at_ms ?? 0),
    }) : undefined,
    registeredAtMs: BigInt(row.registered_at_ms),
    lastSeenMs: BigInt(row.last_seen_ms),
    reachableAddr: row.reachable_addr ?? undefined,
    keeperStale: row.keeper_stale ?? undefined,
  });
}

export function taskRowToProto(row: {
  id: string; state: string; payload_json: string;
  enqueued_at_ms: number;
  claimed_at_ms: number | null; claimed_by: string | null;
  finished_at_ms: number | null; result_json: string | null;
  completion_check: string | null;
  completion_check_last_attempt_ms: number | null;
  claim_ttl_ms: number;
}): PbTask {
  return create(TaskSchema, {
    id: row.id,
    state: row.state,
    payloadJson: row.payload_json,
    enqueuedAtMs: BigInt(row.enqueued_at_ms),
    claimedAtMs: row.claimed_at_ms != null ? BigInt(row.claimed_at_ms) : undefined,
    claimedBy: row.claimed_by ?? undefined,
    finishedAtMs: row.finished_at_ms != null ? BigInt(row.finished_at_ms) : undefined,
    resultJson: row.result_json ?? undefined,
    completionCheck: row.completion_check ?? undefined,
    completionCheckLastAttemptMs: row.completion_check_last_attempt_ms != null
      ? BigInt(row.completion_check_last_attempt_ms) : undefined,
    claimTtlMs: BigInt(row.claim_ttl_ms),
  });
}


export function mcpRelayRowToProto(row: {
  id: string; label: string; kind: string; config_json: string;
  created_at_ms: number;
}): PbMcpRelay {
  return create(McpRelaySchema, {
    id: row.id,
    label: row.label,
    kind: row.kind,
    configJson: row.config_json,
    createdAtMs: BigInt(row.created_at_ms),
  });
}
