import type { CellGridChunkAssembler, CellGridFrame } from "@roost/shared/cell";
import type { SyncClientFrame } from "@roost/shared/proto/sync_pb";
import type { TerminalGeometry } from "@roost/shared/viewport";
import type { CellGridRenderer } from "../lib/cellRenderer.ts";

export type TerminalViewHandleStatus =
  | {
      status: "pending";
      revision: bigint;
      active: boolean;
    }
  | {
      status: "accepted";
      revision: bigint;
      active: boolean;
      streamId: string;
      effectiveCols: number;
      effectiveRows: number;
      baselineReady: boolean;
    }
  | {
      status: "unavailable" | "rejected";
      revision: bigint;
      active: boolean;
      streamId: string;
      effectiveCols: number;
      effectiveRows: number;
      reason: string;
    };

export interface TerminalRendererDelivery {
  frame: CellGridFrame;
  full: boolean;
}

export interface TerminalViewHandle {
  readonly sessionId: string;
  readonly viewId: string;
  setViewport(geometry: TerminalGeometry): void;
  setInactive(): void;
  refresh(): void;
  subscribeStatus(listener: (status: TerminalViewHandleStatus) => void): () => void;
  subscribeRenderer(
    renderer: CellGridRenderer,
    onDelivery?: (delivery: TerminalRendererDelivery) => void,
  ): () => void;
  dispose(): void;
}

export interface TerminalViewIntent {
  revision: bigint;
  active: boolean;
  cols: number;
  rows: number;
}

export interface TerminalViewRecord {
  session: TerminalSessionReplica;
  viewId: string;
  revisionFloor: bigint;
  desired: TerminalViewIntent | null;
  accepted: TerminalViewIntent | null;
  status: TerminalViewHandleStatus | null;
  statusListeners: Set<(status: TerminalViewHandleStatus) => void>;
  rendererSubscribers: Set<TerminalRendererSubscriber>;
  rollingBack: boolean;
  heartbeat: ReturnType<typeof setInterval>;
  leaseDeadlineMs: number | null;
  disposed: boolean;
}

export interface TerminalRendererSubscriber {
  renderer: CellGridRenderer;
  onDelivery: ((delivery: TerminalRendererDelivery) => void) | undefined;
  streamId: string | null;
  gridEpoch: string | null;
  seq: number | null;
}

export interface TerminalSessionReplica {
  sessionId: string;
  handles: Map<string, TerminalViewRecord>;
  subscribers: Set<TerminalRendererSubscriber>;
  expectedStreamId: string | null;
  effectiveCols: number;
  effectiveRows: number;
  canonical: CellGridFrame | null;
  baselineReady: boolean;
  requiresFreshBaseline: boolean;
  resyncLatched: boolean;
  resyncSentGeneration: string | null;
  assembler: CellGridChunkAssembler;
  chunkTimer: ReturnType<typeof setTimeout> | null;
  wireStreamId: string | null;
  wireGridEpoch: string | null;
  wireSeq: number | null;
}

export interface TerminalStreamDiagnosticSnapshot {
  view: {
    view_id: string | null;
    revision: string | null;
    active: boolean;
    status: TerminalViewHandleStatus["status"] | null;
    stream_id: string | null;
    effective_cols: number | null;
    effective_rows: number | null;
    lease_deadline_ms: number | null;
  };
  replica: {
    expected_stream_id: string | null;
    grid_epoch: string | null;
    seq: number | null;
    baseline_ready: boolean;
    resync_latched: boolean;
  };
  wire_received: {
    stream_id: string | null;
    grid_epoch: string | null;
    seq: number | null;
  };
  sync: {
    socket_generation: number | null;
    socket_id: string | null;
    process_epoch: string | null;
    domain_generation: string | null;
    ready: boolean;
  };
}

export type TerminalOutboundCommand = SyncClientFrame["command"];

