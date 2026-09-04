// Initial lists must publish against the exact Sync generation that requested them.
// This module installs those RPC hydrators and converts coordinator protos into store records.
// sync-bootstrap.ts supplies authorization and retry callbacks so socket ordering stays there.
// Live deltas keep using the same wire decoders after these guarded snapshots become ready.

import { reconcile } from "solid-js/store";
import type { Client } from "@connectrpc/connect";
import { SyncDomain } from "@roost/shared/proto/sync_pb";
import type {
  CoordinatorService,
  SessionsListResponse,
} from "@roost/shared/proto/coordinator_pb";
import type {
  McpRelay,
  Session,
  Task,
  Worker,
  Workspace,
} from "@roost/shared/wire";
import { sessionFromProto } from "@roost/shared/wire/session-proto";
import { diag } from "@roost/shared/diag";
import { setRootStore } from "./root.ts";
import type { PairRequest } from "./root.ts";
import { setRoutableFps } from "./sync-routable.ts";
import { applySessionsSnapshot } from "./projector.ts";
import { mcpRelayProtoToWire, taskProtoToWire } from "./sync-proto-adapters.ts";
import { setSessionsHydrated, setTerminalBootstrapStage } from "./sync-hydrated.ts";
import { registerSyncDomainHydrator } from "./sync-domain-hydration.ts";
import type { SyncDomainToken } from "./sync-link-state.ts";

type CoordinatorClient = Client<typeof CoordinatorService>;

interface BootstrapDomainHydrationDeps {
  readonly coordClient: CoordinatorClient;
  readonly selfHosted: boolean;
  readonly onTerminalFailure: (reason: unknown) => Promise<void>;
  readonly onTerminalSnapshotApplied: (
    token: SyncDomainToken,
    sessionCount: number,
  ) => void;
  readonly requestReconnect: () => void;
}

export function _installBootstrapDomainHydrators(
  deps: BootstrapDomainHydrationDeps,
): () => void {
  const unregisterHydrators: Array<() => void> = [];
  unregisterHydrators.push(registerSyncDomainHydrator(SyncDomain.TERMINAL, async (token) => {
    let response: SessionsListResponse;
    try {
      response = await deps.coordClient.sessionsList({ syncSocketId: token.socketId });
    } catch (reason) {
      await deps.onTerminalFailure(reason);
      return null;
    }
    if (!response.syncSnapshotToken) {
      diag("sync.snapshot_token_missing", { domain: "terminal" });
      deps.requestReconnect();
      return null;
    }
    const sessions: Record<string, Session> = {};
    for (const session of response.sessions) {
      try {
        sessions[session.id] = sessionFromProto(session);
      } catch (error) {
        console.warn("[sync.bootstrap] session_from_proto_failed", session.id, error);
        diag("sync.session_from_proto_failed", {
          error: String(error),
          sid: session.id,
        });
      }
    }
    return {
      snapshotToken: response.syncSnapshotToken,
      apply: () => {
        // Rehydration must retain same-id proxies so mounted terminals survive.
        applySessionsSnapshot(sessions);
        setSessionsHydrated(true);
        setTerminalBootstrapStage("ready");
        deps.onTerminalSnapshotApplied(token, Object.keys(sessions).length);
      },
    };
  }));

  unregisterHydrators.push(registerSyncDomainHydrator(SyncDomain.WORKERS, async () => {
    const response = await deps.coordClient.workersList({});
    const workers: Record<string, Worker> = {};
    for (const worker of response.workers) {
      workers[worker.fp] = {
        fp: worker.fp as never,
        label: worker.label,
        os: worker.os as never,
        git_sha: worker.gitSha ?? null,
        host_metrics: worker.hostMetrics ? {
          cpu_pct: worker.hostMetrics.cpuPct,
          mem_used_bytes: Number(worker.hostMetrics.memUsedBytes),
          mem_total_bytes: Number(worker.hostMetrics.memTotalBytes),
          disk_used_bytes: Number(worker.hostMetrics.diskUsedBytes),
          disk_total_bytes: Number(worker.hostMetrics.diskTotalBytes),
          net_rx_bps: Number(worker.hostMetrics.netRxBps),
          net_tx_bps: Number(worker.hostMetrics.netTxBps),
          sampled_at_ms: Number(worker.hostMetrics.sampledAtMs),
        } : null,
        registered_at_ms: Number(worker.registeredAtMs),
        last_seen_ms: Number(worker.lastSeenMs),
        reachable_addr: worker.reachableAddr ?? null,
        keeper_stale: worker.keeperStale ?? null,
      };
    }
    return {
      apply: () => {
        setRoutableFps(new Set(response.routableFps));
        setRootStore("workers", workers);
      },
    };
  }));

  unregisterHydrators.push(registerSyncDomainHydrator(SyncDomain.WORKSPACES, async () => {
    const response = await deps.coordClient.workspacesList({});
    const workspaces: Record<string, Workspace> = {};
    for (const workspace of response.workspaces) {
      workspaces[workspace.id] = {
        id: workspace.id as never,
        worker_fp: workspace.workerFp as never,
        name: workspace.name,
        folder_path: workspace.folderPath,
        color: workspace.color ?? null,
        position: workspace.position,
        version: Number(workspace.version),
        created_at_ms: Number(workspace.createdAtMs),
        updated_at_ms: Number(workspace.updatedAtMs),
        session_ids: workspace.sessionIds as never,
      };
    }
    return { apply: () => setRootStore("workspaces", workspaces) };
  }));

  unregisterHydrators.push(registerSyncDomainHydrator(SyncDomain.TASKS, async () => {
    const response = await deps.coordClient.tasksList({});
    const tasks: Record<string, Task> = {};
    for (const task of response.tasks) {
      // A malformed JSON column drops its row rather than retrying the domain forever.
      const wire = taskProtoToWire(task);
      if (wire) tasks[task.id] = wire;
    }
    return { apply: () => setRootStore("tasks", tasks) };
  }));

  unregisterHydrators.push(registerSyncDomainHydrator(SyncDomain.MCP, async () => {
    const response = await deps.coordClient.mcpList({});
    const relays: Record<string, McpRelay> = {};
    for (const relay of response.relays) {
      const wire = mcpRelayProtoToWire(relay);
      if (wire) relays[relay.id] = wire;
    }
    return { apply: () => setRootStore("mcp_relays", relays) };
  }));

  unregisterHydrators.push(registerSyncDomainHydrator(SyncDomain.PAIR, async () => {
    if (!deps.selfHosted) {
      return {
        apply: () => setRootStore("pair_requests", reconcile({})),
      };
    }
    const response = await deps.coordClient.pairList({});
    const requests: Record<string, PairRequest> = {};
    for (const request of response.requests) {
      requests[request.ephemeralId] = {
        ephemeral_id: request.ephemeralId,
        label: request.label,
        created_at_ms: Number(request.createdAtMs),
      };
    }
    return {
      apply: () => setRootStore("pair_requests", reconcile(requests)),
    };
  }));

  return () => {
    for (const unregister of unregisterHydrators.reverse()) unregister();
  };
}
