// Type surface for coord-link.ts (the worker→coord outbound transport).
// Extracted to keep coord-link.ts under the 400-line cap; re-exported
// from coord-link.ts so external import paths stay unchanged.

import type { PbCellGridChunk, PbCellGridFrame } from "@roost/shared/proto/cell_pb";
import type {
  CoordWorkerDown,
  CoordWorkerUp,
  DAgentPrompt,
  DInputRequest,
  DKeeperUpdatePrepare,
  DTerminalPipelineSnapshotRequest,
  DTerminalSnapshotRequest,
  DTerminalStreamState,
  TerminalInputStatus,
  TerminalStreamFailureKind,
  TerminalStreamStatus,
  TerminalWritePhase,
  WTerminalPipelineSnapshot,
} from "@roost/shared/proto/worker_transport_pb";
import type { AgentStatusUpdate, WorkerFp, ClientControlFrame, SessionEvent } from "@roost/shared/wire";
import type { SessionEventStore } from "./session-event-store.ts";

/** Bounded, monotonic budget for one downstream terminal-control request.
 * The coordinator sends a RELATIVE `budget_ms`, never an instant, and the
 * worker measures elapsed time from frame receipt with its own monotonic
 * clock — so the two hosts' wall clocks may differ by any amount without
 * changing when a request expires or how its outcome is classified.
 *
 * Both members are live and MUST be re-read immediately before the keeper
 * write, never snapshotted at entry: the shared keeper-admission lane can hold
 * a request for a while, and expiry observed there is what makes the resulting
 * rejection provably pre-write. */
export interface TerminalRequestBudget {
  /** Milliseconds left before the coordinator stops waiting; negative once past. */
  remainingMs(): number;
  /** False once the socket that delivered this request has been replaced, so
   * its result can no longer reach the coordinator that asked for it. */
  isCurrentConnection(): boolean;
}

export type WorkerSnapshotEvent = Extract<SessionEvent, { kind: "snapshot" }>;
export type WorkerSnapshotProvider = () => WorkerSnapshotEvent;
export type CoordLinkProtocolPhase = "hello" | "replay" | "snapshot" | "live";

// ─── deps + options ──────────────────────────────────────────────────

export interface CoordLinkDeps {
  // Tail/coord URL e.g. "https://<coord-host>.<tailnet>.ts.net:4102".
  coordHttpUrl: string;
  workerFp: WorkerFp;
  workerVersion: string;
  sessionEventStore: SessionEventStore;
  mintJwt: () => Promise<string>;
  jwtTtlSecs?: number;
  // Test-only overrides for the stale-link watchdog (defaults in
  // coord-link-constants.ts).
  staleLinkTimeoutMs?: number;
  staleCheckIntervalMs?: number;
  // Test-only socket injection for deterministic outbox/backpressure coverage.
  webSocketFactory?: (url: string, protocols: [string, string]) => WebSocket;
  // Fired after native WebSocket backpressure clears and earlier durable events
  // have drained. It runs before queued controls so an authoritative cell
  // repair preserves opened → full → reply ordering.
  // Edge-triggered only after a cell send reported "dropped".
  onWritable?: () => void;
  // Hello establishes a negotiated transport capability set before the worker
  // chooses its terminal metadata encoding.
  onHelloAck?: (msg: { reconnected: boolean; terminalMetadataNegotiated: boolean }) => void;
  // Socket-open observation only. Application traffic remains fenced until
  // onSnapshotReady, which fires after the exact snapshot ACK.
  onOpen?: (reconnected: boolean) => void;
  // Fired synchronously when the active native transport detaches, before a
  // replacement generation can negotiate its capabilities.
  onDetach?: () => void;
  onSnapshotReady?: (msg: { reconnected: boolean }) => void;
  onBrowserCommand?: (msg: { browser_id: string; viewer_id: string; request_id: string; frame: ClientControlFrame }) => void;
  onBinary?: (channelId: number, dir: number, bytes: Uint8Array) => void;
  onInputRequest?: (request: DInputRequest, budget: TerminalRequestBudget) => Promise<void> | void;
  onAgentPrompt?: (request: DAgentPrompt, budget: TerminalRequestBudget) => Promise<void> | void;
  onTerminalStreamState?: (request: DTerminalStreamState, budget: TerminalRequestBudget) => Promise<void> | void;
  onTerminalSnapshotRequest?: (request: DTerminalSnapshotRequest) => Promise<void> | void;
  onTerminalPipelineSnapshot?: (request: DTerminalPipelineSnapshotRequest) => void;
  onKeeperUpdatePrepare?: (
    request: DKeeperUpdatePrepare,
  ) => Promise<{
    outcome: string;
    keeper_pid?: number;
    keeper_epoch?: string;
    binding_digest?: string;
  }> | {
    outcome: string;
    keeper_pid?: number;
    keeper_epoch?: string;
    binding_digest?: string;
  };
  onAttachmentChunk?: (msg: { request_id: string; session_id: string; filename: string; short_path: boolean; data: Uint8Array; last: boolean; seq: number }) => void;
  onUpdateBroker?: (msg: {
    request_id: string;
    job_id: string;
    action: "START" | "STATUS";
    manifest_url: string;
    signature_url: string;
    manifest_sha256: string;
    publisher_sha256: string;
  }) => Promise<readonly UpdateProgressFrame[]> | readonly UpdateProgressFrame[];
}

export interface UpdateProgressFrame {
  request_id: string;
  job_id: string;
  sequence: number;
  phase: string;
  message: string;
  terminal: boolean;
  success: boolean;
  error?: string;
}
export type TransportSendResult = "sent" | "queued" | "dropped";
/** Cell frames are admitted directly to the wire or rejected for retry.
 * Unlike generic control/raw sends, cells never use queue admission as an
 * emission receipt: a snapshot cursor advances only on "sent". */
export type TerminalCellSendResult = "sent" | "dropped";

/** Bounded semantic state for one channel's latest terminal metadata update. */
export interface TerminalMetadataFrame {
  channelId: number;
  titleChanged: boolean;
  title: string;
  activityChanged: boolean;
  activityTsMs: number;
}

export interface CoordLinkPipelineState {
  queueFrames: number;
  queueBytes: number;
  nativeBufferedBytes: number;
  attached: boolean;
}


export interface CoordLink {
  send(frame: UpstreamFrame): boolean;
  sendBinary(channelId: number, direction: number, endSeq: number, data: Uint8Array): TransportSendResult;
  sendTerminalMetadata(metadata: TerminalMetadataFrame): TransportSendResult;
  sendCellGrid(channelId: number, frame: PbCellGridFrame): TerminalCellSendResult;
  /** True when written or retained by the ordered in-memory status repair lane. */
  sendAgentStatus(status: AgentStatusUpdate): boolean;
  state(): CoordLinkState;
  protocolPhase(): CoordLinkProtocolPhase;
  ready(): boolean;
  waitForDurableSessionEventReplay(signal?: AbortSignal): Promise<void>;
  activateSnapshotProvider(provider: WorkerSnapshotProvider): void;
  snapshotStateChanged(): void;
  sendCellGridChunk(channelId: number, chunk: PbCellGridChunk): TerminalCellSendResult;
  dispose(): void;
  pipelineState(): CoordLinkPipelineState;
}

// Canonical worker→coord control-frame shape consumed by callers
// (session-manager.ts, event-sink.ts, snapshot.ts). frameToProto
// converts to wire CoordWorkerUp before sending on the WebSocket.
export type UpstreamFrame =
  | { kind: "hello"; worker_fp: string; version: string; capabilities?: readonly string[] }
  | { kind: "pong"; ts: number }
  | {
      kind: "event";
      event: SessionEvent;
      clientSeq: number;
      eventClass: "durable" | "metadata";
      metadataKey?: string;
    }
  | { kind: "rpc-ok"; request_id: string; data: unknown }
  | { kind: "rpc-error"; request_id: string; message: string }
  // `phase` is mandatory on both result frames: the coordinator honours a
  // REJECTED status as definite — unwinding provisional state and freeing the
  // browser to retry — only when the phase proves the keeper never wrote.
  | {
      kind: "input-result";
      request_id: string;
      session_id: string;
      input_seq: bigint;
      status: TerminalInputStatus;
      written_bytes: number;
      phase: TerminalWritePhase;
      reason?: string;
    }
  | {
      kind: "terminal-stream-result";
      request_id: string;
      session_id: string;
      stream_id: string;
      enabled: boolean;
      status: TerminalStreamStatus;
      channel_resize_seq: bigint;
      effective_cols: number;
      effective_rows: number;
      resized: boolean;
      phase: TerminalWritePhase;
      failure_kind: TerminalStreamFailureKind;
      reason?: string;
    }
  | ({ kind: "update-progress" } & UpdateProgressFrame)
  | {
      kind: "terminal-pipeline-snapshot";
      snapshot: WTerminalPipelineSnapshot;
    };

export type CoordLinkState =
  | { kind: "idle" }
  | { kind: "connecting"; attempt: number }
  | { kind: "open"; since: number }
  | { kind: "reconnecting"; nextDialAtMs: number; backoffMs: number }
  | { kind: "closed" };

// ─── internal seams ──────────────────────────────────────────────────
// The three contracts below are private to this directory: coord-link.ts
// composes an outbox, a reconnect ladder and a downstream dispatcher out of
// the same per-link closure state. They live here rather than in the modules
// that implement them so the seams can be read in one place.

/** Encoded-outbox + native-backpressure engine (coord-link-outbox.ts). It
 * enforces the hello → replay → snapshot → live application barrier while
 * allowing only pong/JWT refresh liveness traffic around it. */
export interface CoordLinkOutbox {
  send(frame: UpstreamFrame): boolean;
  sendBinary(channelId: number, direction: number, endSeq: number, data: Uint8Array): TransportSendResult;
  sendTerminalMetadata(metadata: TerminalMetadataFrame): TransportSendResult;
  sendCellGrid(channelId: number, frame: PbCellGridFrame): TerminalCellSendResult;
  /** True when written or retained by the ordered in-memory status repair lane. */
  sendAgentStatus(status: AgentStatusUpdate): boolean;
  sendCellGridChunk(channelId: number, chunk: PbCellGridChunk): TerminalCellSendResult;
  sendControlProto(frame: CoordWorkerUp): TransportSendResult;
  sendLivenessProto(frame: CoordWorkerUp): TransportSendResult;
  encodeUpstream(frame: CoordWorkerUp): Uint8Array | null;
  /** Bypasses byte admission only for hello on a just-opened socket. */
  forceWrite(bytes: Uint8Array): boolean;
  attachSocket(socket: WebSocket, write: (bytes: Uint8Array) => void): void;
  detachSocket(): void;
  acceptHelloAck(reconnected: boolean, terminalMetadataNegotiated?: boolean): void;
  activateSnapshotProvider(provider: WorkerSnapshotProvider): void;
  snapshotStateChanged(): void;
  protocolPhase(): CoordLinkProtocolPhase;
  ready(): boolean;
  waitForDurableSessionEventReplay(signal?: AbortSignal): Promise<void>;
  isAttached(): boolean;
  activeSocket(): WebSocket | null;
  pipelineState(): CoordLinkPipelineState;
  drainQueues(): void;
  clearDrainTimer(): void;
  ackEvent(seq: number): void;
  reset(): void;
}

/** What the reconnect ladder (coord-link-reconnect.ts) needs from the link it
 *  is redialing. `dial` is fire-and-forget: the ladder never awaits a dial.
 *  `jitter`/`rng` tune the shared backoffDelayMs: production defaults to equal
 *  jitter over Math.random; tests inject both for deterministic ladders. */
export interface CoordLinkReconnectHooks {
  isDisposed(): boolean;
  dial(): void;
  setState(next: CoordLinkState): void;
  jitter?: "equal" | "full" | "none";
  rng?: () => number;
}

/** Exponential-backoff dial timer plus the dial/open counters its cap is
 * derived from (coord-link-reconnect.ts). */
export interface CoordLinkReconnect {
  scheduleReconnect(): void;
  /** Cancels a pending backoff dial. An uncancelled timer means a second
   * concurrent socket. */
  cancelPendingDial(): void;
  /** Counts one dial attempt and returns the number to report in state(). */
  beginDial(): number;
  /** Records a successful open; returns true when this is a RE-open. */
  noteOpen(): boolean;
  noteDialClosed(): void;
  noteStableSession(): void;
}

/** Coordinator→worker frame dispatch (coord-link-downstream.ts). */
export interface CoordLinkDownstream {
  handleDownstream(frame: CoordWorkerDown, reconnected: boolean, socket: WebSocket): void;
}
