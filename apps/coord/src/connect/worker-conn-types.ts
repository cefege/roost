// Shared contracts keep worker connection orchestration, frame dispatch, and
// the WebSocket facade on one dependency vocabulary. This type-only module
// avoids runtime cycles while preserving both established facade import paths.

import type { CoordConfig } from "@roost/shared/config";
import type {
  CoordWorkerUp,
} from "@roost/shared/proto/worker_transport_pb";
import type { CoordinatorWriteGate } from "../coordinator-write-gate.ts";
import type { KyselyDB } from "../db/connection.ts";
import type { PendingEventPublicationStore } from "../pending-event-publications.ts";
import type { JwtCache } from "../jwt.ts";
import type { SelfHostedTenant } from "../self-hosted-tenant.ts";

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
  /** Required: frame dispatch withholds durable ACKs and respawn waits on
   * this exact instance while a keeper update holds the fence. */
  writeGate: CoordinatorWriteGate;
  /** Required: the single self-hosted account/organization/dashboard resolved
   * once at startup. Every scoped write takes its value from here. */
  selfHostedTenant: SelfHostedTenant;
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
