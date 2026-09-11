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
import { hostIdentityToProto } from "../host-identity-proto.ts";
import { safeJsonParse } from "../json.ts";
import {
  keeperRuntimeObservationToProto,
} from "../keeper-update-proto.ts";
import {
  KeeperRuntimeObservationV1Schema,
  type KeeperRuntimeObservationV1,
} from "../keeper-update.ts";
import { terminalCoreCapacityReportToProto } from "../terminal-core-capacity-proto.ts";
import {
  TerminalCoreCapacityReportSchema,
  type TerminalCoreCapacityReport,
} from "../terminal-core-capacity.ts";
import { normalizeHostIdentity, type HostIdentity } from "./worker.ts";

// Wire-shape (Zod) Worker payload for presenceBus.publish. Used by
// workersRegister / workersHeartbeat / workersRename — three near-
// identical inline blocks before this helper existed.
export interface WireWorkerPresence {
  fp: string; label: string; os: string;
  host_identity: HostIdentity | null;
  git_sha: string | null;
  host_metrics: unknown;
  registered_at_ms: number; last_seen_ms: number;
  reachable_addr: string | null;
  keeper_runtime: KeeperRuntimeObservationV1 | null;
  terminal_core_capacity: TerminalCoreCapacityReport | null;
}
export function workerRowToWirePresence(row: {
  fp: string; label: string; os: string; git_sha: string | null;
  host_metrics_json: string | null;
  registered_at_ms: number; last_seen_ms: number;
  reachable_addr: string | null;
  host_identity_json?: string | null;
  keeper_runtime_json?: string | null;
  terminal_core_capacity_json?: string | null;
}): WireWorkerPresence {
  return {
    fp: row.fp, label: row.label, os: row.os,
    host_identity: hostIdentityFromJson(row.host_identity_json),
    git_sha: row.git_sha ?? null,
    host_metrics: safeJsonParse(row.host_metrics_json, null, "host_metrics_json"),
    registered_at_ms: row.registered_at_ms,
    last_seen_ms: row.last_seen_ms,
    reachable_addr: row.reachable_addr ?? null,
    keeper_runtime: keeperRuntimeFromJson(row.keeper_runtime_json),
    terminal_core_capacity: terminalCoreCapacityFromJson(
      row.terminal_core_capacity_json,
    ),
  };
}

export function workerRowToProto(row: {
  fp: string; label: string; os: string; git_sha: string | null;
  host_metrics_json: string | null;
  registered_at_ms: number; last_seen_ms: number;
  reachable_addr: string | null;
  host_identity_json?: string | null;
  keeper_runtime_json?: string | null;
  terminal_core_capacity_json?: string | null;
}): PbWorker {
  const hostMetricsRaw: any = safeJsonParse(row.host_metrics_json, null, "host_metrics_json");
  const keeperRuntime = keeperRuntimeFromJson(row.keeper_runtime_json);
  const terminalCoreCapacity = terminalCoreCapacityFromJson(
    row.terminal_core_capacity_json,
  );
  const hostIdentity = hostIdentityFromJson(row.host_identity_json);
  return create(WorkerSchema, {
    fp: row.fp,
    label: row.label,
    os: row.os,
    hostIdentity: hostIdentity ? hostIdentityToProto(hostIdentity) : undefined,
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
    keeperRuntime: keeperRuntime
      ? keeperRuntimeObservationToProto(keeperRuntime)
      : undefined,
    terminalCoreCapacity: terminalCoreCapacity
      ? terminalCoreCapacityReportToProto(terminalCoreCapacity)
      : undefined,
  });
}

function hostIdentityFromJson(
  serialized: string | null | undefined,
): HostIdentity | null {
  if (!serialized) return null;
  return normalizeHostIdentity(
    safeJsonParse(serialized, null, "host_identity_json"),
  );
}

function keeperRuntimeFromJson(
  serialized: string | null | undefined,
): KeeperRuntimeObservationV1 | null {
  if (!serialized) return null;
  const candidate = safeJsonParse(serialized, null, "keeper_runtime_json");
  const parsed = KeeperRuntimeObservationV1Schema.safeParse(candidate);
  return parsed.success ? parsed.data : null;
}

function terminalCoreCapacityFromJson(
  serialized: string | null | undefined,
): TerminalCoreCapacityReport | null {
  if (!serialized) return null;
  const candidate = safeJsonParse(
    serialized,
    null,
    "terminal_core_capacity_json",
  );
  const parsed = TerminalCoreCapacityReportSchema.safeParse(candidate);
  return parsed.success ? parsed.data : null;
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
