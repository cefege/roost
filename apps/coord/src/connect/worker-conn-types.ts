// Shared contracts keep worker connection orchestration, frame dispatch, and
// the WebSocket facade on one dependency vocabulary. This type-only module
// avoids runtime cycles while preserving both established facade import paths.

import type { CoordConfig } from "@roost/shared/config";
import type {
  CoordWorkerUp,
} from "@roost/shared/proto/worker_transport_pb";
import type { CoordinatorMoveService } from "../coord-move/orchestrator.ts";
import type { KyselyDB } from "../db/connection.ts";
import type { PendingEventPublicationStore } from "../pending-event-publications.ts";
import type { JwtCache } from "../jwt.ts";

export interface WorkerUpdateProgress {
  request_id: string;
  job_id: string;
  sequence: number;
  phase: string;
  message: string;
  terminal: boolean;
  success: boolean;
  error?: string;
}

export interface WorkerServiceDeps {
  db: KyselyDB;
  pendingPublications: PendingEventPublicationStore;
  jwtCache: JwtCache;
  cfg: CoordConfig;
  move?: CoordinatorMoveService;
  onWorkerConnected?: (workerFp: string) => Promise<void> | void;
  onUpdateProgress?: (
    workerFp: string,
    progress: WorkerUpdateProgress,
  ) => void;
}

export interface WorkerConn {
  handleUpstream(frame: CoordWorkerUp): Promise<void>;
  close(): void;
  /** Synchronously rejects every subsequent frame for this admitted socket.
   * The WebSocket owner closes/detaches its ordered queue in the same fence. */
  revoke(): void;
  /** False once a newer authenticated connection took over or this credential
   * was revoked. Pre-hello, the unfenced socket remains its current generation. */
  isCurrentGeneration(): boolean;
  /** True only for the current, non-revoked generation after its exact
   * snapshot has completed durable publication. */
  isReady(): boolean;
}
