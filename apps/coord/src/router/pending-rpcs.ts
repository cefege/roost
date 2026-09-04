// Coord-side correlation table for browser→worker RPCs that need a
// reply (sessions.spawn returns session_id+channel_id, sessions.attach
// returns replay_offset, etc.). Mutation creates a pending entry,
// sends the browser-command downstream to the worker, awaits the
// promise; worker upstream `rpc-ok`/`rpc-error` resolves it.
//
// Used by Connect session-router mutations and resolved by the worker
// WS upstream rpc_ok/rpc_error frames.
//
// Map keys include the authenticated worker fingerprint so client-supplied
// upload ids cannot replace another worker's pending completion.

import { randomUUID } from "node:crypto";
import { ConnectError, Code } from "@connectrpc/connect";
import { log } from "@roost/shared/log";
import { signal, diag } from "@roost/shared/diag";

const DEFAULT_TIMEOUT_MS = 30_000;

interface PendingEntry {
  requestId: string;
  resolve: (data: unknown) => void;
  reject: (err: Error) => void;
  timer: ReturnType<typeof setTimeout>;
  createdAt: number;
  // The authenticated worker namespace prevents one worker response from
  // settling another worker's request even when client correlation ids match.
  workerFp: string | null;
}

const _pending = new Map<string, PendingEntry>();

function pendingKey(requestId: string, workerFp: string | null): string {
  return JSON.stringify([workerFp, requestId]);
}

function findPending(
  requestId: string,
  workerFp?: string,
): { key: string; entry: PendingEntry } | undefined {
  if (workerFp !== undefined) {
    const workerKey = pendingKey(requestId, workerFp);
    const workerEntry = _pending.get(workerKey);
    if (workerEntry) return { key: workerKey, entry: workerEntry };
    const untaggedKey = pendingKey(requestId, null);
    const untaggedEntry = _pending.get(untaggedKey);
    return untaggedEntry ? { key: untaggedKey, entry: untaggedEntry } : undefined;
  }

  let found: { key: string; entry: PendingEntry } | undefined;
  for (const [key, entry] of _pending) {
    if (entry.requestId !== requestId) continue;
    if (found) return undefined;
    found = { key, entry };
  }
  return found;
}

function takePending(
  requestId: string,
  workerFp?: string,
): PendingEntry | undefined {
  const found = findPending(requestId, workerFp);
  if (!found) return undefined;
  clearTimeout(found.entry.timer);
  _pending.delete(found.key);
  return found.entry;
}

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

/** att1-stream: the chunked-unary upload reuses one worker request id across
 * every chunk. The final completion is isolated by authenticated worker. */
export function createPendingRpcWithId<T = unknown>(
  request_id: string,
  timeoutMs = DEFAULT_TIMEOUT_MS,
  workerFp: string | null = null,
): PendingRpc<T> {
  const key = pendingKey(request_id, workerFp);
  if (_pending.has(key)) {
    throw new ConnectError(
      "request_id is already pending for this worker",
      Code.AlreadyExists,
    );
  }
  const promise = new Promise<T>((resolve, reject) => {
    const timer = setTimeout(() => {
      if (_pending.get(key)?.requestId === request_id && _pending.delete(key)) {
        log.warn("pending-rpcs", "timeout", {
          request_id,
          worker_fp: workerFp,
          timeout_ms: timeoutMs,
        });
        signal("rpc.worker_timeout", {
          request_id,
          worker_fp: workerFp,
          waited_ms: timeoutMs,
          cooldownKey: workerFp ?? "worker-rpc",
        });
        reject(new ConnectError(
          `worker did not reply within ${timeoutMs}ms`,
          Code.DeadlineExceeded,
        ));
      }
    }, timeoutMs);
    timer.unref?.();
    _pending.set(key, {
      requestId: request_id,
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
  let count = 0;
  for (const [key, entry] of _pending) {
    if (entry.workerFp !== workerFp) continue;
    clearTimeout(entry.timer);
    _pending.delete(key);
    entry.reject(new ConnectError(message, Code.Unavailable));
    count++;
  }
  if (count > 0) {
    log.warn("pending-rpcs", "rejected_on_worker_close", {
      worker_fp: workerFp,
      count,
    });
    diag("rpc.rejected_worker_close", { worker_fp: workerFp, count });
  }
  return count;
}

export function resolvePendingRpc(
  request_id: string,
  data: unknown,
  workerFp?: string,
): boolean {
  const entry = takePending(request_id, workerFp);
  if (!entry) return false;
  entry.resolve(data);
  return true;
}

export function rejectPendingRpc(
  request_id: string,
  message: string,
  workerFp?: string,
): boolean {
  const entry = takePending(request_id, workerFp);
  if (!entry) return false;
  // The worker's rpc-error carries the real permanent command failure.
  entry.reject(new ConnectError(message || "worker rpc failed", Code.Internal));
  return true;
}

/** Reject a request that never reached the worker because its transport was
 * unavailable. Callers may safely retry these. */
export function rejectPendingRpcUnavailable(
  request_id: string,
  message: string,
  workerFp?: string,
): boolean {
  const entry = takePending(request_id, workerFp);
  if (!entry) return false;
  entry.reject(new ConnectError(
    message || "worker transport unavailable",
    Code.Unavailable,
  ));
  return true;
}

export function _pendingRpcStats(): { pending: number } {
  return { pending: _pending.size };
}
