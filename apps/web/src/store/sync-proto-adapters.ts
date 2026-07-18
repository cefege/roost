// proto→wire adapters for the typed bus deltas carried on the Sync stream.
// Each converts a generated protobuf delta (oneof `kind`) into the legacy
// snake_case wire shape the _handle* projectors in sync.ts already consume,
// so adding a typed bus didn't force a projector rewrite. Pure functions,
// no module state. Sole caller: store/sync.ts::_runConnectSync. Split out of
// sync.ts (400-line cap); behavior unchanged.

function _hmProtoToWire(m: any) {
  return m ? {
    cpu_pct: m.cpuPct,
    mem_used_bytes: Number(m.memUsedBytes), mem_total_bytes: Number(m.memTotalBytes),
    disk_used_bytes: Number(m.diskUsedBytes), disk_total_bytes: Number(m.diskTotalBytes),
    net_rx_bps: Number(m.netRxBps), net_tx_bps: Number(m.netTxBps),
    sampled_at_ms: Number(m.sampledAtMs),
  } : null;
}
function _wsProtoToWire(w: any) {
  return {
    id: w.id, worker_fp: w.workerFp, name: w.name, folder_path: w.folderPath,
    color: w.color ?? null, position: w.position, version: Number(w.version),
    created_at_ms: Number(w.createdAtMs), updated_at_ms: Number(w.updatedAtMs),
    session_ids: w.sessionIds,
  };
}
export function _workspaceProtoToWire(d: any) {
  const v = d.kind?.value;
  switch (d.kind?.case) {
    case "created":   return { kind: "created", workspace: _wsProtoToWire(v) };
    case "updated":   return { kind: "updated", workspace: _wsProtoToWire(v) };
    case "deletedId": return { kind: "deleted", id: v };
    case "sessionsSet": return { kind: "sessions-set", id: v.workspaceId, session_ids: v.sessionIds, version: Number(v.version) };
  }
  return null;
}
export function _taskProtoToWire(d: any) {
  const v = d.kind?.value;
  const taskWire = v ? {
    id: v.id, state: v.state, payload: JSON.parse(v.payloadJson),
    enqueued_at_ms: Number(v.enqueuedAtMs),
    claimed_at_ms: v.claimedAtMs != null ? Number(v.claimedAtMs) : null,
    claimed_by: v.claimedBy ?? null,
    finished_at_ms: v.finishedAtMs != null ? Number(v.finishedAtMs) : null,
    result: v.resultJson ? JSON.parse(v.resultJson) : null,
    completion_check: v.completionCheck ?? null,
    completion_check_last_attempt_ms: v.completionCheckLastAttemptMs != null ? Number(v.completionCheckLastAttemptMs) : null,
    claim_ttl_ms: Number(v.claimTtlMs),
  } : null;
  switch (d.kind?.case) {
    case "created": return { kind: "created", task: taskWire };
    case "state":   return { kind: "state",   task: taskWire };
  }
  return null;
}
export function _webhookProtoToWire(d: any): unknown {
  const v = d.kind?.value;
  switch (d.kind?.case) {
    case "created": return { kind: "created", token: {
      id: v.id, label: v.label, last4: v.last4, scopes: v.scopes,
      created_at_ms: Number(v.createdAtMs),
      last_used_at_ms: v.lastUsedAtMs != null ? Number(v.lastUsedAtMs) : null,
    }};
    case "deletedId": return { kind: "deleted", id: v };
  }
  return null;
}
export function _permProtoToWire(d: any) {
  const v = d.kind?.value;
  const ruleWire = v ? {
    id: v.id, tool_pattern: v.toolPattern, folder_glob: v.folderGlob,
    decision: v.decision, enabled: v.enabled, created_at_ms: Number(v.createdAtMs),
  } : null;
  switch (d.kind?.case) {
    case "created": return { kind: "created", rule: ruleWire };
    case "updated": return { kind: "updated", rule: ruleWire };
    case "deletedId": return { kind: "deleted", id: v };
  }
  return null;
}
export function _mcpProtoToWire(d: any) {
  const v = d.kind?.value;
  switch (d.kind?.case) {
    case "created":
    case "updated": return { kind: d.kind.case, relay: {
      id: v.id, label: v.label, kind: v.kind,
      config: JSON.parse(v.configJson),
      created_at_ms: Number(v.createdAtMs),
    }};
    case "deletedId": return { kind: "deleted", id: v };
    case "event": return { relay_id: v.relayId, payload: JSON.parse(v.payloadJson), ts: Number(v.ts) };
  }
  return null;
}
export function _presenceProtoToWire(d: any) {
  const v = d.kind?.value;
  switch (d.kind?.case) {
    case "registered": return {
      kind: "registered",
      worker: {
        fp: v.fp, label: v.label, os: v.os, git_sha: v.gitSha ?? null,
        host_metrics: _hmProtoToWire(v.hostMetrics),
        registered_at_ms: Number(v.registeredAtMs),
        last_seen_ms: Number(v.lastSeenMs),
        reachable_addr: v.reachableAddr ?? null,
        keeper_stale: v.keeperStale ?? null,
      },
    };
    case "heartbeat": return {
      kind: "heartbeat", fp: v.workerFp, last_seen_ms: Number(v.lastSeenMs),
      host_metrics: _hmProtoToWire(v.hostMetrics),
    };
    case "removedFp": return { kind: "removed", fp: v };
  }
  return null;
}
