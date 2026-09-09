import type { CellGridChunkAssembler, CellGridFrame } from "@roost/shared/cell";
import type { SyncClientFrame } from "@roost/shared/proto/sync_pb";
import type { TerminalGeometry } from "@roost/shared/viewport";
import type { CellGridRenderer } from "../lib/cellRenderer.ts";
import type { TerminalRenderScheduler } from "../lib/terminal-render-scheduler.ts";

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
export type TerminalRendererForegroundPredicate = () => boolean;

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
export interface TerminalGenerationToken {
  readonly socketGeneration: number;
  readonly socketId: string;
  readonly processEpoch: string;
  readonly domainGeneration: bigint;
}
export interface TerminalGenerationDiagnosticToken {
  readonly socketGeneration: number;
  readonly socketId: string;
  readonly processEpoch: string;
  readonly domainGeneration: string;
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
  challengeLiveness(): void;
  setViewport(geometry: TerminalGeometry): void;
  setInactive(): void;
  refresh(): void;
  subscribeStatus(listener: (status: TerminalViewHandleStatus) => void): () => void;
  /** Attach-progress stream for this view's session replica. Emits the
   * current value immediately, then only on assembler or replica transitions;
   * null clears any determinate bar. */
  subscribeProgress(listener: (progress: BaselineProgress | null) => void): () => void;
  subscribeRenderer(
    renderer: CellGridRenderer,
    onDelivery?: (delivery: TerminalRendererDelivery) => void,
    isForeground?: TerminalRendererForegroundPredicate,
  ): () => void;
  dispose(): void;
}

export interface TerminalViewIntent {
  revision: bigint;
  active: boolean;
  cols: number;
  rows: number;
}
export type TerminalRepairOutcome =
  | "none"
  | "requested"
  | "proved"
  | "escalated"
  | "generation_reset"
  | "inactive"
  | "disposed"
  | "stream_replaced"
  | "pruned";


export interface TerminalViewRecord {
  session: TerminalSessionReplica;
  viewId: string;
  revisionFloor: bigint;
  desired: TerminalViewIntent | null;
  accepted: TerminalViewIntent | null;
  status: TerminalViewHandleStatus | null;
  statusListeners: Set<(status: TerminalViewHandleStatus) => void>;
  progressListeners: Set<(progress: BaselineProgress | null) => void>;
  lastProgressKey: string | null;
  rendererSubscribers: Set<TerminalRendererSubscriber>;
  rollingBack: boolean;
  viewAckTimer: Timer | null;
  renewalDueAtMs: number | null;
  leaseDeadlineMs: number | null;
  pendingViewAckAtMs: number | null;
  pendingViewAckGeneration: TerminalGenerationToken | null;
  pendingViewAckRevision: bigint | null;
  disposed: boolean;
}

export interface TerminalRendererSubscriber {
  sessionId: string;
  scheduler: TerminalRenderScheduler;
  isForeground: TerminalRendererForegroundPredicate;
  viewActive: boolean;
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
  generation: TerminalGenerationToken | null;
  lastAcceptedFrameAtMs: number | null;
  idleProbeTimer: Timer | null;
  proofDeadlineTimer: Timer | null;
  lastAcceptedFrameGeneration: TerminalGenerationToken | null;
  proofChallengeAtMs: number | null;
  proofChallengeGeneration: TerminalGenerationToken | null;
  resyncLatchGeneration: TerminalGenerationToken | null;
  resyncLatchedAtMs: number | null;
  repairAttempts: number;
  repairOutcome: TerminalRepairOutcome;
  assembler: CellGridChunkAssembler;
  chunkTimer: Timer | null;
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
    pending_ack_age_ms: number | null;
    pending_ack_generation: TerminalGenerationDiagnosticToken | null;
  };
  replica: {
    expected_stream_id: string | null;
    grid_epoch: string | null;
    seq: number | null;
    baseline_ready: boolean;
    resync_latched: boolean;
    last_terminal_proof_age_ms: number | null;
    last_terminal_proof_generation: TerminalGenerationDiagnosticToken | null;
    challenge_age_ms: number | null;
    challenge_generation: TerminalGenerationDiagnosticToken | null;
    resync_latch_age_ms: number | null;
    resync_latch_generation: TerminalGenerationDiagnosticToken | null;
    repair_attempts: number;
    repair_outcome: TerminalRepairOutcome;
  };
  wire_received: {
    stream_id: string | null;
    grid_epoch: string | null;
    seq: number | null;
  };
  faults: {
    blackhole_drop_count: number;
    wire_delta_drop_count: number;
    wire_delta_dropped_seq: number | null;
    wire_delta_post_drop_seq: number | null;
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

