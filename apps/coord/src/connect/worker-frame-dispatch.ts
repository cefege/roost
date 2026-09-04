// Dispatches worker frames that mutate durable or live coordinator state.
// makeWorkerConn performs connection admission and generation checks first,
// then supplies guarded readiness and delayed-respawn capabilities here.
// Keeping these effects together preserves append, publish, and ACK order.

import { create } from "@bufbuild/protobuf";
import {
  CoordWorkerDownSchema,
  DEventAckSchema,
  type CoordWorkerDown,
  type CoordWorkerUp,
  type WSessionEvent,
} from "@roost/shared/proto/worker_transport_pb";
import { asChannelId, asWorkerFp } from "@roost/shared/wire";
import { protoToEvent } from "@roost/shared/wire/event-proto";
import { log } from "@roost/shared/log";
import { diag, signal } from "@roost/shared/diag";
import { handleWorkerAgentStatus } from "../agent-status-hub.ts";
import {
  publishBytes,
  publishCellGrid,
  publishCellGridChunk,
} from "../byte-hub.ts";
import { appendEvent, dispatchSnapshotOrphanReaps } from "../event-log.ts";
import { rejectPendingRpc, resolvePendingRpc } from "../router/pending-rpcs.ts";
import {
  hasPendingSpawn,
  resolvePendingSpawnOpened,
} from "./pending-spawns.ts";
import type { WorkerServiceDeps } from "./worker-conn-types.ts";

interface WorkerFrameDispatcherOptions {
  deps: WorkerServiceDeps;
  callerFingerprint: string;
  dashboardId?: string;
  requestClose(): void;
  getWorkerFp(): string | null;
  isSnapshotReady(): boolean;
  isCurrentGeneration(): boolean;
  fenced(what: string): boolean;
  sendBestEffort(what: string, frame: CoordWorkerDown): boolean;
  markSnapshotReady(): boolean;
  scheduleRespawn(workerFp: string): void;
}

export function makeWorkerFrameDispatcher(options: WorkerFrameDispatcherOptions) {
  async function handleEvent(workerEvent: WSessionEvent): Promise<void> {
    const workerFp = options.getWorkerFp();
    // Dedup uses (worker_fp, client_seq); a pre-hello row would have no worker
    // identity and would fall outside the partial uniqueness index.
    if (!workerFp) {
      log.warn("worker-service", "event_before_hello", {});
      diag("worker.frame_dropped", {
        reason: "event_before_hello",
        worker_fp: options.callerFingerprint,
      });
      signal("worker.protocol_violation", {
        reason: "event_before_hello",
        worker_fp: options.callerFingerprint,
        cooldownKey: options.callerFingerprint,
      });
      options.requestClose();
      return;
    }
    if (options.fenced("event")) return;
    const rawEvent = workerEvent as { event?: unknown; clientSeq?: bigint };
    const clientSeq = rawEvent.clientSeq === undefined
      ? 0
      : Number(rawEvent.clientSeq);
    if (!Number.isSafeInteger(clientSeq) || clientSeq <= 0) {
      diag("worker.frame_dropped", {
        reason: "invalid_event_client_seq",
        worker_fp: workerFp,
      });
      return;
    }
    let event;
    try {
      event = protoToEvent(rawEvent.event as never);
    } catch (error) {
      log.warn("worker-service", "event_decode_failed", { error: String(error) });
      diag("worker.frame_dropped", { reason: "event_decode_failed", worker_fp: workerFp });
      signal("worker.protocol_violation", {
        reason: "event_decode_failed",
        worker_fp: workerFp,
        cooldownKey: workerFp,
      });
      return;
    }
    if (!event) {
      log.warn("worker-service", "event_decode_returned_null", {});
      diag("worker.frame_dropped", {
        reason: "event_decode_returned_null",
        worker_fp: workerFp,
      });
      signal("worker.protocol_violation", {
        reason: "event_decode_returned_null",
        worker_fp: workerFp,
        cooldownKey: workerFp,
      });
      return;
    }
    if (
      !options.isSnapshotReady()
      && event.kind !== "opened"
      && event.kind !== "closed"
      && event.kind !== "respawned"
      && event.kind !== "snapshot"
    ) {
      diag("worker.frame_dropped", {
        reason: "event_before_snapshot_ready",
        event_kind: event.kind,
        worker_fp: workerFp,
      });
      return;
    }
    if (
      options.deps.move?.gate.mode !== undefined
      && options.deps.move.gate.mode !== "active"
    ) {
      // Pending targets and draining/retired sources keep the worker link alive
      // but withhold the ACK so CoordLink replays this preserved entry later.
      return;
    }
    const lease = options.deps.move?.gate.acquire();
    let appendResult;
    try {
      appendResult = await appendEvent(options.deps.db, event, {
        worker_fp: workerFp,
        client_seq: clientSeq,
        dashboardId: options.dashboardId,
        allowNewWorkerSession: options.deps.cfg?.saasMode
          ? hasPendingSpawn
          : undefined,
        requireExistingWorkerSessions: options.deps.cfg?.saasMode === true,
        canPublish: options.isCurrentGeneration,
        pendingPublications: options.deps.pendingPublications,
        deferSnapshotReap: event.kind === "snapshot",
      });
    } catch (error) {
      log.error("worker-service", "event_append_failed", {
        worker_fp: workerFp,
        kind: event.kind,
        client_seq: clientSeq,
        error: String(error),
      });
      signal("event.append_failed", {
        error: String(error),
        worker_fp: workerFp,
        cooldownKey: "events",
      });
      throw error;
    } finally {
      lease?.release();
    }
    if (!appendResult.admitted) {
      diag("worker.frame_dropped", {
        reason: "event_scope_unproven",
        event_kind: event.kind,
        worker_fp: workerFp,
      });
      return;
    }
    if (options.fenced("event_post_commit")) return;
    if (appendResult.replayRejected) {
      log.warn("worker-service", "event_dedupe_payload_mismatch", {
        worker_fp: workerFp,
        client_seq: clientSeq,
      });
      signal("worker.protocol_violation", {
        reason: "event_dedupe_payload_mismatch",
        worker_fp: workerFp,
        cooldownKey: workerFp,
      });
      options.requestClose();
      return;
    }
    const committedEvent = appendResult.event;
    if (committedEvent.kind === "snapshot") {
      // A dedupe/stale generation did not install this connection's exact live
      // set. It cannot cross readiness and receives no ACK.
      if (!appendResult.published) return;
      const becameReady = options.markSnapshotReady();
      if (clientSeq > 0) {
        options.sendBestEffort("event_ack", create(CoordWorkerDownSchema, {
          frame: {
            case: "eventAck",
            value: create(DEventAckSchema, { clientSeq: BigInt(clientSeq) }),
          },
        }));
      }
      dispatchSnapshotOrphanReaps(
        asWorkerFp(workerFp),
        appendResult.snapshotReapIds,
      );
      if (becameReady) {
        void Promise.resolve(options.deps.onWorkerConnected?.(workerFp)).catch((error) => {
          log.warn("worker-service", "connected_callback_failed", {
            worker_fp: workerFp,
            error: String(error),
          });
        });
        options.scheduleRespawn(workerFp);
      }
      return;
    }
    if (
      committedEvent.kind === "opened"
      && appendResult.dashboardId !== null
    ) {
      resolvePendingSpawnOpened(
        appendResult.dashboardId,
        workerFp,
        committedEvent.session_id,
        Number(committedEvent.channel),
      );
    }
    if (clientSeq > 0) {
      options.sendBestEffort("event_ack", create(CoordWorkerDownSchema, {
        frame: {
          case: "eventAck",
          value: create(DEventAckSchema, { clientSeq: BigInt(clientSeq) }),
        },
      }));
    }
  }

  function pendingResultWorker(frameKind: string): string | null {
    const workerFp = options.getWorkerFp();
    if (
      !workerFp
      || !options.isSnapshotReady()
      || options.fenced(frameKind)
    ) {
      diag("worker.frame_dropped", {
        reason: "unready_rpc_result",
        what: frameKind,
        worker_fp: workerFp ?? options.callerFingerprint,
      });
      return null;
    }
    return workerFp;
  }

  function handleLiveFrame(frame: CoordWorkerUp): boolean {
    const workerFp = options.getWorkerFp();
    switch (frame.frame.case) {
      case "binary": {
        const binary = frame.frame.value;
        if (workerFp && !options.fenced("binary")) {
          publishBytes(
            asWorkerFp(workerFp),
            asChannelId(binary.channelId),
            binary.data,
          );
        }
        return true;
      }
      case "cellGrid": {
        const cellGrid = frame.frame.value;
        if (workerFp && cellGrid.frame && !options.fenced("cell_grid")) {
          publishCellGrid(
            asWorkerFp(workerFp),
            asChannelId(cellGrid.channelId),
            cellGrid.frame,
          );
        }
        return true;
      }
      case "cellGridChunk": {
        const cellGrid = frame.frame.value;
        if (workerFp && cellGrid.chunk && !options.fenced("cell_grid_chunk")) {
          publishCellGridChunk(
            asWorkerFp(workerFp),
            asChannelId(cellGrid.channelId),
            cellGrid.chunk,
          );
        }
        return true;
      }
      case "agentStatus": {
        if (!workerFp) {
          diag("worker.frame_dropped", {
            reason: "agent_status_before_hello",
            worker_fp: options.callerFingerprint,
          });
          options.requestClose();
          return true;
        }
        if (options.fenced("agent_status")) return true;
        const status = frame.frame.value;
        handleWorkerAgentStatus(workerFp, {
          session_id: status.sessionId,
          agent_id: status.agentId,
          state: status.state,
          message: status.message,
          revision: Number(status.revision),
          completed_revision: Number(status.completedRevision),
          updated_at: status.updatedAt,
          active: status.active,
        });
        return true;
      }
      case "terminalStreamResult": {
        const resultWorkerFp = pendingResultWorker("terminal_stream_result");
        if (resultWorkerFp) {
          resolvePendingRpc(
            frame.frame.value.requestId,
            frame.frame.value,
            resultWorkerFp,
          );
        }
        return true;
      }
      case "inputResult": {
        const resultWorkerFp = pendingResultWorker("input_result");
        if (resultWorkerFp) {
          resolvePendingRpc(
            frame.frame.value.requestId,
            frame.frame.value,
            resultWorkerFp,
          );
        }
        return true;
      }
      case "updateProgress": {
        if (!workerFp) {
          diag("worker.frame_dropped", {
            reason: "update_progress_before_hello",
            worker_fp: options.callerFingerprint,
          });
          options.requestClose();
          return true;
        }
        const progress = frame.frame.value;
        const sequence = Number(progress.sequence);
        if (!progress.jobId || !Number.isSafeInteger(sequence) || sequence < 0) {
          diag("worker.frame_dropped", {
            reason: "invalid_update_progress",
            worker_fp: workerFp,
          });
          return true;
        }
        options.deps.onUpdateProgress?.(workerFp, {
          request_id: progress.requestId,
          job_id: progress.jobId,
          sequence,
          phase: progress.phase,
          message: progress.message,
          terminal: progress.terminal,
          success: progress.success,
          error: progress.error || undefined,
        });
        return true;
      }
      case "rpcOk": {
        const resultWorkerFp = pendingResultWorker("rpc_ok");
        if (!resultWorkerFp) return true;
        try {
          resolvePendingRpc(
            frame.frame.value.requestId,
            JSON.parse(frame.frame.value.dataJson),
            resultWorkerFp,
          );
        } catch {
          // A malformed response cannot satisfy a pending RPC.
        }
        return true;
      }
      case "rpcError": {
        const resultWorkerFp = pendingResultWorker("rpc_error");
        if (resultWorkerFp) {
          rejectPendingRpc(
            frame.frame.value.requestId,
            frame.frame.value.message,
            resultWorkerFp,
          );
        }
        return true;
      }
      default:
        return false;
    }
  }

  return { handleEvent, handleLiveFrame };
}
