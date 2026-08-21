// Outbound worker frames: the socket-shape shim the router/files/scrollback
// use, plus the browser-command and attachment-chunk senders. Each resolves
// the target worker through the shared connectWorkers registry and writes a
// CoordWorkerDown frame on its live send handle.

import { create } from "@bufbuild/protobuf";
import { Code, ConnectError } from "@connectrpc/connect";
import {
  CoordWorkerDownSchema, DBrowserCommandSchema, DBinarySchema, DAttachmentChunkSchema,
  DCoordMovePrepareSchema, DCoordMoveSnapshotStartSchema, DCoordMoveSnapshotChunkSchema, DCoordRelocateSchema,
  DInputRequestSchema, DTerminalStreamStateSchema, DTerminalSnapshotRequestSchema,
  type WInputResult, type WTerminalStreamResult,
  DUpdateBrokerSchema,
} from "@roost/shared/proto/worker_transport_pb";
import { connectWorkers } from "./worker-registry.ts";
import { createPendingRpc, rejectPendingRpcUnavailable } from "../router/pending-rpcs.ts";
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

export {
  collectWorkerDiagSnapshots,
  type WorkerDiagSnapshotErrorCode,
  type WorkerDiagSnapshotResult,
} from "./worker-diag-snapshot.ts";

// ── monotonic hop budgets ────────────────────────────────────────────
//
// Every terminal hop is bounded by a RELATIVE budget measured from a local
// monotonic origin; no absolute instant crosses the wire. The coordinator's
// and the worker's wall clocks may therefore differ by any amount — including
// a step or an NTP slew mid-request — without changing which side expires or
// how an outcome is classified.
//
// The budgets nest strictly:
//   keeper reconciliation (6s, worker)
//     < worker pre-write budget (coordinator remaining − WORKER_HOP_RESERVE_MS)
//       < coordinator result deadline (5s input / 8s stream-state)
// so an inner expiry always reports back while its outer waiter is still
// listening, and an outer expiry can never race an inner one into a
// fabricated verdict.
export const INPUT_CONTROL_TIMEOUT_MS = 5_000;
// Bound the coordinator lane independently of the worker's 6s keeper budget.
// Expiry is post-admission ambiguity, never a fabricated rejection.
export const TERMINAL_STREAM_CONTROL_TIMEOUT_MS = 8_000;
// Return-trip + decode headroom withheld from the worker's slice so a worker
// pre-write rejection still arrives before the coordinator stops waiting.
const WORKER_HOP_RESERVE_MS = 750;
// Below this, the remaining budget cannot survive the hop, so writing the
// frame buys nothing but duplicate risk. Refusing here is provably clean:
// nothing was sent, so nothing can have mutated.
const MIN_WORKER_BUDGET_MS = 250;

export interface HopDeadline {
  /** Whole budget this deadline started with. */
  readonly totalMs: number;
  /** Milliseconds left before the outer waiter gives up; negative once past. */
  remainingMs(): number;
}

/** Start a hop deadline on the monotonic clock. `performance.now()` ignores
 * clock steps, NTP slew, and timezone changes, so a command queued behind a
 * slow lane spends real elapsed time instead of whatever the wall clock did
 * meanwhile — and it cannot be handed a fresh full budget by a backwards step. */
export function startHopDeadline(totalMs: number): HopDeadline {
  const startedAtMono = performance.now();
  return {
    totalMs,
    remainingMs: () => totalMs - (performance.now() - startedAtMono),
  };
}

/** The worker's slice of what is left, or null when too little remains to
 * attempt the hop at all. */
function workerBudgetMs(deadline: HopDeadline): number | null {
  const budget = Math.floor(deadline.remainingMs()) - WORKER_HOP_RESERVE_MS;
  return budget >= MIN_WORKER_BUDGET_MS ? budget : null;
}

export interface TerminalWorkerRequest<T> {
  /** True only when Bun accepted or queued the encoded worker frame. */
  admitted: boolean;
  /** True when the hop budget ran out BEFORE the frame reached the socket.
   * Nothing was written, so the caller may reject definitely and a retry
   * cannot duplicate. Never set once `admitted` is true. */
  expired: boolean;
  /** Correlation id expected in the typed result; absent before allocation. */
  requestId: string | null;
  result: Promise<T>;
}

function unsentRequest<T>(reason: string, expired: boolean): TerminalWorkerRequest<T> {
  return {
    admitted: false,
    expired,
    requestId: null,
    result: Promise.reject(new ConnectError(reason, Code.Unavailable)),
  };
}

/** Send one terminal-input batch and wait for the keeper-completed result.
 * A non-zero Bun send result is transport admission only; the returned promise
 * resolves exclusively from WInputResult after the keeper has written all
 * bytes, rejected before writing, or reported a partial/unknown ambiguity. */
export function sendTerminalInputRequest(
  workerFp: string,
  message: { sessionId: string; inputSeq: bigint; data: Uint8Array },
  deadline: HopDeadline = startHopDeadline(INPUT_CONTROL_TIMEOUT_MS),
): TerminalWorkerRequest<WInputResult> {
  const worker = connectWorkers.get(workerFp);
  if (!worker) return unsentRequest("worker offline", false);
  const budgetMs = workerBudgetMs(deadline);
  if (budgetMs === null) return unsentRequest("terminal input budget expired before send", true);
  const pending = createPendingRpc<WInputResult>(
    Math.max(1, Math.ceil(deadline.remainingMs())),
    workerFp,
  );
  let admitted = false;
  try {
    const sent = worker.send(create(CoordWorkerDownSchema, {
      frame: { case: "inputRequest", value: create(DInputRequestSchema, {
        requestId: pending.request_id,
        sessionId: message.sessionId,
        inputSeq: message.inputSeq,
        data: message.data,
        budgetMs,
      }) },
    }));
    admitted = sent !== 0;
    if (!admitted) {
      rejectPendingRpcUnavailable(pending.request_id, "worker transport dropped terminal input");
    }
  } catch (error) {
    rejectPendingRpcUnavailable(
      pending.request_id,
      error instanceof Error ? error.message : "worker transport failed terminal input",
    );
  }
  return {
    admitted,
    expired: false,
    requestId: pending.request_id,
    result: pending.promise,
  };
}

/** Send one aggregated terminal stream state and wait for the worker's
 * keeper-proof result. Membership is coordinator-local and is never rolled
 * back from this result; the failure kind only drives stream retry/adoption. */
export function sendTerminalStreamStateRequest(
  workerFp: string,
  message: {
    sessionId: string;
    streamId: string;
    enabled: boolean;
    cols: number;
    rows: number;
  },
  deadline: HopDeadline = startHopDeadline(TERMINAL_STREAM_CONTROL_TIMEOUT_MS),
): TerminalWorkerRequest<WTerminalStreamResult> {
  const worker = connectWorkers.get(workerFp);
  if (!worker) return unsentRequest("worker offline", false);
  const budgetMs = workerBudgetMs(deadline);
  if (budgetMs === null) return unsentRequest("terminal stream budget expired before send", true);
  const pending = createPendingRpc<WTerminalStreamResult>(
    Math.max(1, Math.ceil(deadline.remainingMs())),
    workerFp,
  );
  let admitted = false;
  try {
    const sent = worker.send(create(CoordWorkerDownSchema, {
      frame: { case: "terminalStreamState", value: create(DTerminalStreamStateSchema, {
        requestId: pending.request_id,
        sessionId: message.sessionId,
        streamId: message.streamId,
        enabled: message.enabled,
        cols: message.cols,
        rows: message.rows,
        budgetMs,
      }) },
    }));
    admitted = sent !== 0;
    if (!admitted) {
      rejectPendingRpcUnavailable(pending.request_id, "worker transport dropped terminal stream state");
    }
  } catch (error) {
    rejectPendingRpcUnavailable(
      pending.request_id,
      error instanceof Error ? error.message : "worker transport failed terminal stream state",
    );
  }
  return {
    admitted,
    expired: false,
    requestId: pending.request_id,
    result: pending.promise,
  };
}

/** Fire-and-forget full-baseline repair for the currently expected stream. */
export function sendTerminalSnapshotRequest(
  workerFp: string,
  message: { sessionId: string; streamId: string },
): boolean {
  const worker = connectWorkers.get(workerFp);
  if (!worker) return false;
  try {
    return worker.send(create(CoordWorkerDownSchema, {
      frame: {
        case: "terminalSnapshotRequest",
        value: create(DTerminalSnapshotRequestSchema, {
          sessionId: message.sessionId,
          streamId: message.streamId,
        }),
      },
    })) !== 0;
  } catch {
    return false;
  }
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
  if (!worker) throw new ConnectError("worker offline", Code.Unavailable);
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
  if (!worker) throw new ConnectError("worker offline", Code.Unavailable);
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
  if (!worker) throw new ConnectError("worker offline", Code.Unavailable);
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
  if (!worker) throw new ConnectError("worker offline", Code.Unavailable);
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
  if (!worker) throw new ConnectError("worker offline", Code.Unavailable);
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
    if (sent === 0) throw new Error("worker update command was dropped");
  } catch (error) {
    rejectPendingRpcUnavailable(pending.request_id, (error as Error).message);
  }
  return { requestId: pending.request_id, promise: pending.promise };
}
