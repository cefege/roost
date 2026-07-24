// Type surface for CoordLink.ts (the worker→coord outbound transport).
// Extracted to keep CoordLink.ts under the 400-line cap; re-exported
// from CoordLink.ts so external import paths stay unchanged.

import type { PbCellGridFrame } from "@roost/shared/proto/cell_pb";
import type { ChatFrame } from "@roost/shared/chat/wire";
import type { WorkerFp, ClientControlFrame, SessionEvent } from "@roost/shared/wire";

// ─── deps + options ──────────────────────────────────────────────────

export interface CoordLinkDeps {
  // Tail/coord URL e.g. "https://<coord-host>.<tailnet>.ts.net:4102".
  coordHttpUrl: string;
  workerFp: WorkerFp;
  workerVersion: string;
  mintJwt: () => Promise<string>;
  jwtTtlSecs?: number;
  // Test-only overrides for the stale-link watchdog (defaults in
  // CoordLink-constants.ts).
  staleLinkTimeoutMs?: number;
  staleCheckIntervalMs?: number;
  onHelloAck?: (msg: { coord_pubkey_b64: string; coord_pubkey_kid: string }) => void;
  onBrowserCommand?: (msg: { browser_id: string; viewer_id: string; request_id: string; frame: ClientControlFrame }) => void;
  onBinary?: (channelId: number, dir: number, bytes: Uint8Array) => void;
  // Streamed file upload (att1-stream). One call per DAttachmentChunk; the
  // worker assembles by request_id and replies rpc-ok on `last`.
  onAttachmentChunk?: (msg: { request_id: string; session_id: string; filename: string; short_path: boolean; data: Uint8Array; last: boolean; seq: number }) => void;
}

export interface CoordLink {
  send(frame: UpstreamFrame): boolean;
  sendBinary(bytes: Uint8Array | ArrayBufferView): boolean;
  // R11. Volatile — dropped when the stream is down (the worker
  // re-sends a full frame on reconnect/attach), so no pending buffer.
  sendCellGrid(channelId: number, frame: PbCellGridFrame): boolean;
  // Volatile herdr agent status (running|needs-input|idle). Same drop-on-down
  // policy as sendCellGrid — the worker re-scrapes and re-sends on reconnect.
  sendClaudeStatus(channelId: number, status: string): boolean;
  // Omp chat frame (transcript-reader). Volatile — dropped on down stream; the
  // watcher re-seeds via reset + history backfill on reconnect.
  sendChatFrame(channelId: number, frame: ChatFrame): boolean;
  state(): CoordLinkState;
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
  | { kind: "transfer-line"; job_id: string; text: string }
  | { kind: "transfer-done"; job_id: string; exit: number | null; error?: string };

export type CoordLinkState =
  | { kind: "idle" }
  | { kind: "connecting"; attempt: number }
  | { kind: "open"; since: number }
  | { kind: "reconnecting"; nextDialAtMs: number; backoffMs: number }
  | { kind: "closed" };
