// Viewport ownership types for the terminal outbound lane. Split out of
// ws/sync-outbound.ts so the registry (sync-outbound-viewport-registry.ts), the
// wire dispatcher (sync-outbound-viewport-dispatch.ts) and the claim entry point
// (sync-outbound-viewport.ts) can share one declaration without importing each
// other. Types only — no state, no side effects, no imports.

export type ViewportOutcome =
  | {
      status: "accepted";
      sequence: bigint;
      effectiveCols: number;
      effectiveRows: number;
      channelResizeSeq: bigint;
    }
  | { status: "rejected" | "superseded"; sequence: bigint; reason: string };

export interface ViewportAdmission {
  /** Sequence assigned to the first attempt. Retries advance the wire sequence,
   * while the result reports the sequence that ultimately became ready. */
  sequence: bigint;
  result: Promise<ViewportOutcome>;
}

export interface TerminalViewportClaim {
  cols: number;
  rows: number;
  cause: number;
  heldCellSeq?: number | bigint;
  /** Require a full frame newer than this attempt's dispatch receipt before
   * resolving the admission. Current-model claims normally leave this false. */
  repairRequired?: boolean;
}

export interface TerminalViewportFullFrame {
  seq: number;
  gridEpoch: string;
}

export type TerminalViewportOwnerStatus =
  | { status: "pending"; sequence: bigint; repairRequired: boolean }
  | { status: "retrying"; sequence: bigint; reason: string; retryInMs: number }
  | {
      status: "repairing";
      sequence: bigint;
      effectiveCols: number;
      effectiveRows: number;
      channelResizeSeq: bigint;
    }
  | {
      status: "ready";
      sequence: bigint;
      effectiveCols: number;
      effectiveRows: number;
      channelResizeSeq: bigint;
    }
  | { status: "rejected" | "superseded"; sequence: bigint; reason: string };

export type TerminalViewportStatusListener = (status: TerminalViewportOwnerStatus) => void;

export interface TerminalViewportOwner {
  readonly token: bigint;
  claim(value: TerminalViewportClaim): ViewportAdmission;
  heartbeat(heldCellSeq: number | bigint): void;
  noteFullFrame(frame: TerminalViewportFullFrame): void;
  subscribeStatus(listener: TerminalViewportStatusListener): () => void;
  dispose(): void;
}

export interface ViewportDesired {
  sequence: bigint;
  cols: number;
  rows: number;
  cause: number;
  heldCellSeq: bigint;
  repairRequired: boolean;
  updatedAt: number;
  admission: ViewportAdmission;
  resolve: (result: ViewportOutcome) => void;
  settled: boolean;
  retryCount: number;
  needsSequenceAdvance: boolean;
}

export interface ViewportAttempt {
  sequence: bigint;
  socketId: string;
  domainGeneration: bigint;
  processEpoch: string;
  fullFrameReceiptFloor: number;
  fullFrameReady: boolean;
  phase: "result" | "repair";
  deadlineAt: number;
  timer: ReturnType<typeof setTimeout> | null;
  accepted: {
    effectiveCols: number;
    effectiveRows: number;
    channelResizeSeq: bigint;
  } | null;
}

export interface ViewportPreclaim {
  sequence: bigint;
  cols: number;
  rows: number;
  cause: number;
  updatedAt: number;
}

export interface ViewportSession {
  sessionId: string;
  sequenceFloor: bigint;
  sequenceUpdatedAt: number;
  ownerToken: bigint | null;
  desired: ViewportDesired | null;
  attempt: ViewportAttempt | null;
  preclaim: ViewportPreclaim | null;
  confirmed: {
    sequence: bigint;
    socketId: string;
    domainGeneration: bigint;
    processEpoch: string;
    effectiveCols: number;
    effectiveRows: number;
  } | null;
  retryTimer: ReturnType<typeof setTimeout> | null;
  retryAt: number | null;
  retryReason: string | null;
  retrySocketId: string | null;
  retryDomainGeneration: bigint | null;
  processEpoch: string | null;
  fullFrameReceipt: number;
  fullFrameSeq: number;
  fullFrameGridEpoch: string | null;
  status: TerminalViewportOwnerStatus | null;
  listeners: Set<TerminalViewportStatusListener>;
}

/** Bounded diagnostic view of one session's viewport ownership. The `sync`
 * half of TerminalOutboundSnapshot stays with the terminal Sync identity in
 * ws/sync-outbound.ts; this half is owned by the viewport registry. */
export interface TerminalViewportClaimSnapshot {
  owner_token: string | null;
  sequence_floor: string;
  status: TerminalViewportOwnerStatus["status"] | null;
  desired: {
    client_seq: string;
    cols: number;
    rows: number;
    cause: number;
    held_cell_seq: string;
    updated_at_ms: number;
  } | null;
  confirmed: {
    client_seq: string;
    socket_id: string;
    domain_generation: string;
    effective_cols: number;
    effective_rows: number;
  } | null;
  attempt: {
    client_seq: string;
    socket_id: string;
    domain_generation: string;
    phase: "result" | "repair";
    deadline_at_ms: number;
  } | null;
  retry: {
    at_ms: number;
    reason: string;
  } | null;
}
