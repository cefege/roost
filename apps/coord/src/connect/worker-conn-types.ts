// Shared contracts keep worker connection orchestration, frame dispatch, and
// the WebSocket facade on one dependency vocabulary. This type-only module
// avoids runtime cycles while preserving both established facade import paths.

import type { CoordConfig } from "@roost/host/config";
import type {
  CoordWorkerUp,
  WTerminalInputRouteResult,
  WTerminalTransportProbeResult,
} from "@roost/protocol/proto/worker_transport_pb";
import type { CoordinatorWriteGate } from "../coordinator-write-gate.ts";
import type { KyselyDB } from "../db/connection.ts";
import type { PendingEventPublicationStore } from "../pending-event-publications.ts";
import type { JwtCache } from "../jwt.ts";
import type { SelfHostedTenant } from "../self-hosted-tenant.ts";
import type { WorkerHandle } from "./worker-registry.ts";
import type { TerminalPeerNegotiationWorkerResultSink } from "./terminal-peer-negotiation-state.ts";
import type { AttachmentPeerNegotiationWorkerResultSink } from "./attachment-peer-negotiation-state.ts";
import type { AttachmentDirectStatusResultSink } from "./attachment-direct-status-results.ts";

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

export interface TerminalInputRouteResultSink {
  acceptInputRouteResult(source: WorkerHandle, result: WTerminalInputRouteResult): boolean;
  acceptTransportProbeResult(source: WorkerHandle, result: WTerminalTransportProbeResult): boolean;
  cancelForWorkerHandle(worker: WorkerHandle, reason: string): void;
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
  /** Installed only after the composition owner can correlate typed peer results. */
  terminalPeerNegotiations?: TerminalPeerNegotiationWorkerResultSink;
  /** Installed only after attachment-peer results have an exact correlation owner. */
  attachmentPeerNegotiations?: AttachmentPeerNegotiationWorkerResultSink;
  /** Present only while typed attachment status replies have a correlation owner. */
  attachmentDirectStatusResults?: AttachmentDirectStatusResultSink;
  /** Present only when a typed route-result owner can correlate current frames. */
  terminalInputRouteResults?: TerminalInputRouteResultSink;
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
