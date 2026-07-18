// Coord-side correlation table for browser→worker RPCs that need a
// reply (sessions.spawn returns session_id+channel_id, sessions.attach
// returns replay_offset, etc.). Mutation creates a pending entry,
// sends the browser-command downstream to the worker, awaits the
// promise; worker upstream `rpc-ok`/`rpc-error` resolves it.
//
// Used by Connect session-router mutations and resolved by the worker
// WS upstream rpc_ok/rpc_error frames.
//
// Map key is a UUID minted per call; lifecycle is bounded by deadline
// timer so a dead worker doesn't leak entries.

import { randomUUID } from "node:crypto";
import { ConnectError, Code } from "@connectrpc/connect";
import { log } from "@roost/shared/log";
import { signal, diag } from "@roost/shared/diag";

const DEFAULT_TIMEOUT_MS = 30_000;

interface PendingEntry {
  resolve: (data: unknown) => void;
  reject: (err: Error) => void;
  timer: ReturnType<typeof setTimeout>;
  createdAt: number;
  // A5: the worker this RPC was routed to, so its socket close can reject
  // it immediately instead of leaving the browser to hang until the
  // deadline timer (15-30s). null = not worker-routed (shouldn't happen
  // for browser→worker RPCs, but tolerated).
  workerFp: string | null;
}

const _pending = new Map<string, PendingEntry>();

export interface PendingRpc<T = unknown> {
  request_id: string;
  promise: Promise<T>;
}

export function createPendingRpc<T = unknown>(
  timeoutMs = DEFAULT_TIMEOUT_MS,
  workerFp: string | null = null,
): PendingRpc<T> {
  return createPendingRpcWithId<T>(randomUUID(), timeoutMs, workerFp);
}

/** att1-stream: the chunked-unary upload reuses ONE worker request_id across
 *  every chunk (the worker assembles by it). Coord registers the pending under
 *  that shared id on the final chunk so the worker's rpc-ok resolves it. */
export function createPendingRpcWithId<T = unknown>(
  request_id: string,
  timeoutMs = DEFAULT_TIMEOUT_MS,
  workerFp: string | null = null,
): PendingRpc<T> {
  const promise = new Promise<T>((resolve, reject) => {
    const timer = setTimeout(() => {
      if (_pending.delete(request_id)) {
        log.warn("pending-rpcs", "timeout", { request_id, timeoutMs });
        signal("rpc.worker_timeout", { request_id, worker_fp: workerFp, waited_ms: timeoutMs, cooldownKey: workerFp ?? "worker-rpc" });
        // ConnectError (not plain Error): Connect-ES forwards a ConnectError's
        // code+message to the browser verbatim, so the toast reads the real
        // reason instead of the masked "[internal] internal error".
        reject(new ConnectError(`worker did not reply within ${timeoutMs}ms`, Code.DeadlineExceeded));
      }
    }, timeoutMs);
    _pending.set(request_id, {
      resolve: resolve as (data: unknown) => void,
      reject,
      timer,
      createdAt: Date.now(),
      workerFp,
    });
  });
  return { request_id, promise };
}

/** A5: reject every in-flight RPC routed to `workerFp` — called when that
 *  worker's socket closes AND no fresh connection re-registered (guarded
 *  by the caller so a same-fp reconnect's new RPCs aren't cancelled).
 *  Browser fast-fails with a retryable error instead of hanging until the
 *  deadline. Returns the count rejected. */
export function rejectPendingRpcsForWorker(workerFp: string, message: string): number {
  let n = 0;
  for (const [request_id, entry] of _pending) {
    if (entry.workerFp !== workerFp) continue;
    clearTimeout(entry.timer);
    _pending.delete(request_id);
    entry.reject(new ConnectError(message, Code.Unavailable));
    n++;
  }
  if (n > 0) log.warn("pending-rpcs", "rejected_on_worker_close", { workerFp, count: n });
  if (n > 0) diag("rpc.rejected_worker_close", { worker_fp: workerFp, count: n });
  return n;
}

export function resolvePendingRpc(request_id: string, data: unknown): boolean {
  const entry = _pending.get(request_id);
  if (!entry) return false;
  clearTimeout(entry.timer);
  _pending.delete(request_id);
  entry.resolve(data);
  return true;
}

export function rejectPendingRpc(request_id: string, message: string): boolean {
  const entry = _pending.get(request_id);
  if (!entry) return false;
  clearTimeout(entry.timer);
  _pending.delete(request_id);
  // The worker's rpc-error `message` carries the REAL failure (e.g. "keeper
  // spawn no-ack after 800ms", a dead-PTY exit). Reject with ConnectError so
  // Connect-ES relays that text to the browser toast instead of masking a
  // plain Error to "[internal] internal error".
  entry.reject(new ConnectError(message || "worker rpc failed", Code.Internal));
  return true;
}

export function _pendingRpcStats(): { pending: number } {
  return { pending: _pending.size };
}
