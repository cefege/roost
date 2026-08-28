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

/** Chunked-baseline attach progress for one session replica. Mirrors
 * CellGridChunkAssembler.snapshotProgress; null whenever no chunk assembly
 * is in flight (idle, single-frame baseline, completed, or reset). */
export interface BaselineProgress {
  snapshotId: string;
  receivedChunks: number;
  totalChunks: number;
}

export type TerminalPresentationState = "idle" | "receiving" | "catching_up";
export const FRAME_ACTIVITY_WINDOW_MS = 500;

export interface TerminalPresentationWatermark {
  grid_epoch: string | null;
  seq: number | null;
}

export interface TerminalPresentationActivity {
  grid_epoch: string;
  seq: number;
  started_at_ms: number;
}

export function deriveTerminalPresentationState(input: {
  active: boolean;
  acceptedWithBaseline: boolean;
  canonical: TerminalPresentationWatermark;
  reconciled: TerminalPresentationWatermark;
  activity: TerminalPresentationActivity | null;
  nowMs: number;
}): TerminalPresentationState {
  if (!input.active || !input.acceptedWithBaseline) return "idle";
  if (
    input.canonical.grid_epoch !== input.reconciled.grid_epoch
    || input.canonical.seq !== input.reconciled.seq
  ) return "catching_up";
  if (
    input.activity !== null
    && input.activity.grid_epoch === input.canonical.grid_epoch
    && input.activity.seq === input.canonical.seq
    && input.nowMs - input.activity.started_at_ms < FRAME_ACTIVITY_WINDOW_MS
  ) return "receiving";
  return "idle";
}
export interface TerminalViewHandle {
  readonly sessionId: string;
  readonly viewId: string;
  setViewport(geometry: TerminalGeometry): void;
  setInactive(): void;
  refresh(): void;
  subscribeStatus(listener: (status: TerminalViewHandleStatus) => void): () => void;
  /** Attach-progress stream for this view's session replica. Emits the
   * current value immediately, then on every assembler change observed by
   * the view-owned poller; null clears any determinate bar. */
  subscribeProgress(listener: (progress: BaselineProgress | null) => void): () => void;
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
  progressListeners: Set<(progress: BaselineProgress | null) => void>;
  // Attach-progress poll state owned by subscribeProgress in
  // terminal-stream-view.ts; the interval lives only while listeners do.
  progressTimer: ReturnType<typeof setInterval> | null;
  lastProgressKey: string | null;
  rendererSubscribers: Set<TerminalRendererSubscriber>;
  rollingBack: boolean;
  // Liveness re-assert interval; armed immediately after record creation,
  // so null only between the two statements (and never observed).
  heartbeat: ReturnType<typeof setInterval> | null;
  leaseDeadlineMs: number | null;
  disposed: boolean;
}

export interface TerminalRendererSubscriber {
  sessionId: string;
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
  resyncRetryGeneration: string | null;
  resyncRetryAtMs: number | null;
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

