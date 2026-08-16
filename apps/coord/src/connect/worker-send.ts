// Outbound worker frames: the socket-shape shim the router/files/scrollback
// use, plus the browser-command and attachment-chunk senders. Each resolves
// the target worker through the shared connectWorkers registry and writes a
// CoordWorkerDown frame on its live send handle.

import { create } from "@bufbuild/protobuf";
import {
  CoordWorkerDownSchema, DBrowserCommandSchema, DBinarySchema, DAttachmentChunkSchema,
  DCoordMovePrepareSchema, DCoordMoveSnapshotStartSchema, DCoordMoveSnapshotChunkSchema, DCoordRelocateSchema,
  DInputRequestSchema, DViewportRequestSchema,
  type WInputResult, type WViewportResult,
  DUpdateBrokerSchema,
} from "@roost/shared/proto/worker_transport_pb";
import { connectWorkers } from "./worker-registry.ts";
import { createPendingRpc, rejectPendingRpc } from "../router/pending-rpcs.ts";
import { log } from "@roost/shared/log";

/** Socket-shape shim: presents the worker-conn registry to call sites
 * as a `.send(string|Uint8Array)` handle so router.ts/files.ts/scrollback
 * don't have to know which transport is underneath. */
export function getWorkerHubSocket(workerFp: string): { send(data: string | Uint8Array): void } | null {
  const w = connectWorkers.get(workerFp);
  if (!w) return null;
  return {
    send(data: string | Uint8Array): void {
      // Hot path (PTY bytes, browser commands): callers in router/files/
      // scrollback assume this never throws. w.send now surfaces transport
      // failures, so contain them here.
      try {
        if (typeof data === "string") {
          // browser-command JSON envelope: { kind: "browser-command", browser_id, viewer_id, request_id, frame }
          const parsed = JSON.parse(data) as {
            kind: "browser-command";
            browser_id: string; viewer_id: string; request_id: string; frame: unknown;
          };
          w.send(create(CoordWorkerDownSchema, {
            frame: { case: "browserCommand", value: create(DBrowserCommandSchema, {
              browserId: parsed.browser_id,
              viewerId: parsed.viewer_id,
              requestId: parsed.request_id,
              frameJson: JSON.stringify(parsed.frame),
            })},
          }));
        } else {
          // Raw binary frame: [2-byte BE channel][1-byte direction][payload]
          if (data.length < 3) return;
          const channelId = (data[0]! << 8) | data[1]!;
          const direction = data[2]!;
          const payload = data.subarray(3);
          w.send(create(CoordWorkerDownSchema, {
            frame: { case: "binary", value: create(DBinarySchema, {
              channelId, direction, data: payload,
            })},
          }));
        }
      } catch (e) {
        log.warn("worker-send", "hub_send_failed", { worker_fp: workerFp, error: String(e) });
      }
    },
  };
}

/** Send a JSON-encoded ClientControlFrame to a worker as a browser-command.
 * Returns true if the worker is currently connected and the frame was
 * queued for delivery. */
export function sendBrowserCommand(
  workerFp: string,
  msg: { browser_id: string; viewer_id: string; request_id: string; frame: unknown },
): boolean {
  const w = connectWorkers.get(workerFp);
  if (!w) return false;
  try {
    w.send(create(CoordWorkerDownSchema, {
      frame: { case: "browserCommand", value: create(DBrowserCommandSchema, {
        browserId: msg.browser_id,
        viewerId: msg.viewer_id,
        requestId: msg.request_id,
        frameJson: JSON.stringify(msg.frame),
      })},
    }));
    return true;
  } catch { return false; }
}

const INPUT_CONTROL_TIMEOUT_MS = 5_000;
// A keeper-applied resize with a lost ACK must status-reconcile the same
// sequence. Keep the coordinator lane/gate alive through that reconciliation
// rather than fabricating a rejection after the ordinary RPC deadline.
const VIEWPORT_CONTROL_TIMEOUT_MS = 120_000;

export interface TerminalWorkerRequest<T> {
  /** True only when Bun accepted or queued the encoded worker frame. */
  admitted: boolean;
  result: Promise<T>;
}

/** Send one terminal-input batch and wait for the keeper-completed result.
 * A non-zero Bun send result is transport admission only; the returned promise
 * resolves exclusively from WInputResult after the keeper has written all
 * bytes, rejected before writing, or reported a partial/unknown ambiguity. */
export function sendTerminalInputRequest(
  workerFp: string,
  message: { sessionId: string; inputSeq: bigint; data: Uint8Array },
  timeoutMs = INPUT_CONTROL_TIMEOUT_MS,
): TerminalWorkerRequest<WInputResult> {
  const worker = connectWorkers.get(workerFp);
  if (!worker) {
    return { admitted: false, result: Promise.reject(new Error("worker offline")) };
  }
  const pending = createPendingRpc<WInputResult>(timeoutMs, workerFp);
  let admitted = false;
  try {
    const sent = worker.send(create(CoordWorkerDownSchema, {
      frame: { case: "inputRequest", value: create(DInputRequestSchema, {
        requestId: pending.request_id,
        sessionId: message.sessionId,
        inputSeq: message.inputSeq,
        data: message.data,
      }) },
    }));
    admitted = sent !== 0;
    if (!admitted) rejectPendingRpc(pending.request_id, "worker transport dropped terminal input");
  } catch (error) {
    rejectPendingRpc(
      pending.request_id,
      error instanceof Error ? error.message : "worker transport failed terminal input",
    );
  }
  return { admitted, result: pending.promise };
}

/** Send one viewport mutation and wait for a committed/no-op or rejected
 * worker result. A dropped, missing, or late result never becomes acceptance;
 * the caller keeps the browser intent pending and may reconcile the same
 * client sequence. */
export function sendTerminalViewportRequest(
  workerFp: string,
  message: {
    sessionId: string;
    viewerId: string;
    clientSeq: bigint;
    cols: number;
    rows: number;
    cause: number;
    heldCellSeq: bigint;
  },
  timeoutMs = VIEWPORT_CONTROL_TIMEOUT_MS,
): TerminalWorkerRequest<WViewportResult> {
  const worker = connectWorkers.get(workerFp);
  if (!worker) {
    return { admitted: false, result: Promise.reject(new Error("worker offline")) };
  }
  const pending = createPendingRpc<WViewportResult>(timeoutMs, workerFp);
  let admitted = false;
  try {
    const sent = worker.send(create(CoordWorkerDownSchema, {
      frame: { case: "viewportRequest", value: create(DViewportRequestSchema, {
        requestId: pending.request_id,
        sessionId: message.sessionId,
        viewerId: message.viewerId,
        clientSeq: message.clientSeq,
        cols: message.cols,
        rows: message.rows,
        cause: message.cause,
        heldCellSeq: message.heldCellSeq,
      }) },
    }));
    admitted = sent !== 0;
    if (!admitted) rejectPendingRpc(pending.request_id, "worker transport dropped viewport request");
  } catch (error) {
    rejectPendingRpc(
      pending.request_id,
      error instanceof Error ? error.message : "worker transport failed viewport request",
    );
  }
  return { admitted, result: pending.promise };
}

/** att1-stream — relay one streamed-upload chunk to a worker. The first chunk
 *  carries metadata; every chunk carries `data`; `last` triggers rename+reply.
 *  Returns false if the worker isn't connected. */
export function sendAttachmentChunk(
  workerFp: string,
  chunk: { requestId: string; sessionId: string; filename: string; shortPath: boolean; data: Uint8Array; last: boolean; seq: number },
): boolean {
  const w = connectWorkers.get(workerFp);
  if (!w) return false;
  try {
    w.send(create(CoordWorkerDownSchema, {
      frame: { case: "attachmentChunk", value: create(DAttachmentChunkSchema, {
        requestId: chunk.requestId, sessionId: chunk.sessionId, filename: chunk.filename,
        shortPath: chunk.shortPath, data: chunk.data, last: chunk.last, seq: chunk.seq,
      })},
    }));
    return true;
  } catch { return false; }
}

export async function sendCoordinatorMovePrepare(workerFp: string, message: {
  handoffId: string; sourceUrl: string; targetUrl: string; expectedCoordKid: string;
  expectedGitSha: string; estimatedDbSize: bigint; action: "CHECK" | "PREPARE";
}, timeoutMs = 180_000): Promise<unknown> {
  const worker = connectWorkers.get(workerFp);
  if (!worker) throw new Error("worker offline");
  const pending = createPendingRpc(timeoutMs, workerFp);
  worker.send(create(CoordWorkerDownSchema, { frame: { case: "coordMovePrepare", value: create(DCoordMovePrepareSchema, {
    requestId: pending.request_id, ...message,
  }) } }));
  return pending.promise;
}

export async function sendCoordinatorRelocate(workerFp: string, message: {
  handoffId: string; sourceUrl: string; targetUrl: string; action: "STAGE" | "ACTIVATE" | "COMMIT" | "ABORT";
}, timeoutMs = 30_000): Promise<unknown> {
  const worker = connectWorkers.get(workerFp);
  if (!worker) throw new Error("worker offline");
  const pending = createPendingRpc(timeoutMs, workerFp);
  worker.send(create(CoordWorkerDownSchema, { frame: { case: "coordRelocate", value: create(DCoordRelocateSchema, {
    requestId: pending.request_id, ...message,
  }) } }));
  return pending.promise;
}

export function sendCoordinatorSnapshotStart(workerFp: string, message: {
  handoffId: string; totalSize: bigint; sha256: string; coordKeyPem: Uint8Array;
  authorizedKeys: Uint8Array; secretSha256: string; expectedWorkerFps: string[];
}, timeoutMs = 120_000): { requestId: string; promise: Promise<unknown> } {
  const worker = connectWorkers.get(workerFp);
  if (!worker) throw new Error("worker offline");
  const pending = createPendingRpc(timeoutMs, workerFp);
  worker.send(create(CoordWorkerDownSchema, { frame: { case: "coordMoveSnapshotStart", value: create(DCoordMoveSnapshotStartSchema, {
    requestId: pending.request_id, ...message,
  }) } }));
  return { requestId: pending.request_id, promise: pending.promise };
}
export function sendCoordinatorSnapshotChunk(workerFp: string, message: {
  handoffId: string; seq: number; data: Uint8Array; last: boolean;
}): number {
  const worker = connectWorkers.get(workerFp);
  if (!worker) throw new Error("worker offline");
  return worker.send(create(CoordWorkerDownSchema, { frame: { case: "coordMoveSnapshotChunk", value: create(DCoordMoveSnapshotChunkSchema, message) } }));
}

export function sendWindowsUpdateBroker(workerFp: string, message: {
  jobId: string;
  action: "START" | "STATUS";
  manifestUrl?: string;
  signatureUrl?: string;
  manifestSha256?: string;
  publisherSha256?: string;
}, timeoutMs = 30_000): { requestId: string; promise: Promise<unknown> } {
  const worker = connectWorkers.get(workerFp);
  if (!worker) throw new Error("worker offline");
  const pending = createPendingRpc(timeoutMs, workerFp);
  try {
    const sent = worker.send(create(CoordWorkerDownSchema, {
      frame: { case: "updateBroker", value: create(DUpdateBrokerSchema, {
        requestId: pending.request_id,
        jobId: message.jobId,
        action: message.action,
        manifestUrl: message.manifestUrl ?? "",
        signatureUrl: message.signatureUrl ?? "",
        manifestSha256: message.manifestSha256 ?? "",
        publisherSha256: message.publisherSha256 ?? "",
      }) },
    }));
    if (sent <= 0) throw new Error("worker update command was not queued");
  } catch (error) {
    rejectPendingRpc(pending.request_id, (error as Error).message);
  }
  return { requestId: pending.request_id, promise: pending.promise };
}
