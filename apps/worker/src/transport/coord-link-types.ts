// Type surface for coord-link.ts (the worker→coord outbound transport).
// Extracted to keep coord-link.ts under the 400-line cap; re-exported
// from coord-link.ts so external import paths stay unchanged.

import type { PbCellGridFrame } from "@roost/shared/proto/cell_pb";
import type { CoordWorkerUp, CoordWorkerDown } from "@roost/shared/proto/worker_transport_pb";
import type {
  DInputRequest,
  DViewportRequest,
  TerminalInputStatus,
  TerminalViewportStatus,
  TerminalWritePhase,
} from "@roost/shared/proto/worker_transport_pb";
import type { AgentStatusUpdate, WorkerFp, ClientControlFrame, SessionEvent } from "@roost/shared/wire";

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

// ─── deps + options ──────────────────────────────────────────────────

export interface CoordLinkDeps {
  // Tail/coord URL e.g. "https://<coord-host>.<tailnet>.ts.net:4102".
  coordHttpUrl: string;
  workerFp: WorkerFp;
  workerVersion: string;
  mintJwt: () => Promise<string>;
  jwtTtlSecs?: number;
  // Test-only overrides for the stale-link watchdog (defaults in
  // coord-link-constants.ts).
  staleLinkTimeoutMs?: number;
  staleCheckIntervalMs?: number;
  // Test-only socket injection for deterministic outbox/backpressure coverage.
  webSocketFactory?: (url: string) => WebSocket;
  // Fired after native WebSocket backpressure clears and earlier durable events
  // have drained. It runs before queued controls so an authoritative cell
  // repair preserves opened → full → reply ordering.
  // Edge-triggered only after a cell send reported "dropped".
  onWritable?: () => void;
  onHelloAck?: (msg: { coord_pubkey_b64: string; coord_pubkey_kid: string; reconnected: boolean }) => void;
  // Fires after hello + event/control drain on every successful socket open.
  // Raw metadata remains held until helloAck and authoritative cell repair.
  // reconnected=false only for the first open in this CoordLink lifetime.
  onOpen?: (reconnected: boolean) => void;
  onBrowserCommand?: (msg: { browser_id: string; viewer_id: string; request_id: string; frame: ClientControlFrame }) => void;
  onBinary?: (channelId: number, dir: number, bytes: Uint8Array) => void;
  onInputRequest?: (request: DInputRequest, budget: TerminalRequestBudget) => Promise<void> | void;
  onViewportRequest?: (request: DViewportRequest, budget: TerminalRequestBudget) => Promise<void> | void;
  onAttachmentChunk?: (msg: { request_id: string; session_id: string; filename: string; short_path: boolean; data: Uint8Array; last: boolean; seq: number }) => void;
  onCoordMovePrepare?: (msg: {
    request_id: string; handoff_id: string; source_url: string; target_url: string;
    expected_coord_kid: string; expected_git_sha: string; estimated_db_size: bigint; action: "CHECK" | "PREPARE";
  }) => Promise<void> | void;
  onCoordMoveSnapshotStart?: (msg: {
    request_id: string; handoff_id: string; total_size: bigint; sha256: string; coord_key_pem: Uint8Array;
    authorized_keys: Uint8Array; secret_sha256: string; expected_worker_fps: string[];
  }) => Promise<void> | void;
  onCoordMoveSnapshotChunk?: (msg: { handoff_id: string; seq: number; data: Uint8Array; last: boolean }) => Promise<void> | void;
  onCoordRelocate?: (msg: {
    request_id: string; handoff_id: string; source_url: string; target_url: string; action: "STAGE" | "ACTIVATE" | "COMMIT" | "ABORT";
  }) => Promise<void> | void;
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


export interface CoordLink {
  send(frame: UpstreamFrame): boolean;
  sendBinary(channelId: number, direction: number, endSeq: number, data: Uint8Array): TransportSendResult;
  sendCellGrid(channelId: number, frame: PbCellGridFrame): TransportSendResult;
  sendAgentStatus(status: AgentStatusUpdate): boolean;
  state(): CoordLinkState;
  relocate(targetUrl: string, force?: boolean): void;
  unackedEventCount(): number;
  dispose(): void;
}

// Canonical worker→coord control-frame shape consumed by callers
// (session-manager.ts, event-sink.ts, snapshot.ts). frameToProto
// converts to wire CoordWorkerUp before sending on the WebSocket.
export type UpstreamFrame =
  | { kind: "hello"; worker_fp: string; version: string }
  | { kind: "pong"; ts: number }
  | { kind: "event"; event: SessionEvent }
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
      kind: "viewport-result";
      request_id: string;
      session_id: string;
      client_seq: bigint;
      status: TerminalViewportStatus;
      channel_resize_seq: bigint;
      cols: number;
      rows: number;
      resized: boolean;
      phase: TerminalWritePhase;
      reason?: string;
      sequence_floor?: bigint;
    }
  | { kind: "transfer-line"; job_id: string; text: string }
  | { kind: "transfer-done"; job_id: string; exit: number | null; error?: string }
  | ({ kind: "update-progress" } & UpdateProgressFrame);

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

/** Encoded-outbox + native-backpressure engine (coord-link-outbox.ts). Owns
 * every byte that leaves the worker, the D-4b unacked SessionEvent outbox and
 * the socket currently attached to it. */
export interface CoordLinkOutbox {
  send(frame: UpstreamFrame): boolean;
  sendBinary(channelId: number, direction: number, endSeq: number, data: Uint8Array): TransportSendResult;
  sendCellGrid(channelId: number, frame: PbCellGridFrame): TransportSendResult;
  sendAgentStatus(status: AgentStatusUpdate): boolean;
  sendControlProto(frame: CoordWorkerUp): TransportSendResult;
  encodeUpstream(frame: CoordWorkerUp): Uint8Array | null;
  /** Bypasses byte admission. Legitimate only for the hello frame on a
   * just-opened socket, whose native buffer is provably empty. */
  forceWrite(bytes: Uint8Array): boolean;
  /** Installs the writer for a freshly opened socket. Leaves the link
   * not-ready: raw metadata stays held until markLinkReady(). */
  attachSocket(socket: WebSocket, write: (bytes: Uint8Array) => void): void;
  detachSocket(): void;
  markLinkReady(): void;
  isAttached(): boolean;
  activeSocket(): WebSocket | null;
  replayUnacked(): void;
  drainQueues(): void;
  clearDrainTimer(): void;
  ackEvent(seq: number): void;
  unackedCount(): number;
  reset(): void;
}

/** What the reconnect ladder (coord-link-reconnect.ts) needs from the link it
 * is redialling. `dial` is fire-and-forget: the ladder never awaits a dial. */
export interface CoordLinkReconnectHooks {
  isDisposed(): boolean;
  dial(): void;
  setState(next: CoordLinkState): void;
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
  resetForRedial(): void;
}

/** Coordinator→worker frame dispatch (coord-link-downstream.ts). */
export interface CoordLinkDownstream {
  handleDownstream(frame: CoordWorkerDown, reconnected: boolean, socket: WebSocket): void;
}
