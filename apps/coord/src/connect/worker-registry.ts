// Worker-connection registry: the shared Map of currently-attached workers
// keyed by fingerprint, plus the routable-set accessors the router and the
// SPA online indicator read. Populated/cleared by makeWorkerConn
// (worker-conn.ts) as raw WSs attach/close.

import type { CoordWorkerDown } from "@roost/shared/proto/worker_transport_pb";
import { workerRoutableBus } from "../buses.ts";
import { diag } from "@roost/shared/diag";

export interface WorkerHandle {
  workerFp: string;
  /** Persisted at worker bootstrap and fixed for this authenticated connection.
   * Undefined is permitted only by legacy test seams; production admission
   * never registers an unscoped worker. */
  dashboardId?: string;
  /** Synchronous credential fence. Once set, this handle can never admit or
   *  send another authoritative frame, even if a caller retained the object. */
  revoked: boolean;
  /** Snapshot barrier. A current authenticated socket remains unroutable until
   * its exact snapshot has committed and finished durable publication. */
  ready: boolean;
  /** Transport-owned inbound cleanup (ordered queue + announcement barrier).
   *  Called synchronously after `revoked` is set and before registry removal. */
  fence?: () => void;
  /** Bun ws.send result: 0 = dropped, -1 = enqueued under backpressure, >0 = bytes. */
  send: (frame: CoordWorkerDown) => number;
  /** Closes only this transport connection; used to replay held events at commit. */
  close?: () => void;
  /** Bytes still queued on the socket; drives snapshot-pump backpressure. */
  bufferedAmount?: () => number;
}

// Registry of currently-attached workers, keyed by fp. A hello claims the
// generation immediately, but the handle is authoritative/routable only after
// its exact snapshot has committed and set `ready`.
export const connectWorkers = new Map<string, WorkerHandle>();

/** Test-only seam: install/remove a fake WorkerHandle without the full raw-WS
 * attach handshake. Focused routing tests use it to prove worker command
 * admission and response correlation. Production code never calls this. */
export function __setConnectWorkerForTest(
  workerFp: string,
  handle: (Omit<WorkerHandle, "revoked" | "ready"> & {
    revoked?: boolean;
    ready?: boolean;
  }) | null,
): void {
  if (handle) {
    handle.revoked ??= false;
    handle.ready ??= true;
    connectWorkers.set(workerFp, handle as WorkerHandle);
  } else {
    connectWorkers.delete(workerFp);
  }
}

/** Permanently fence the current connection generation for a consumed worker
 * credential. The ordering is load-bearing: mark revoked, detach inbound work,
 * then unregister. Routability publication is a separate best-effort cleanup. */
export function fenceWorkerCredential(workerFp: string): WorkerHandle | null {
  const handle = connectWorkers.get(workerFp);
  if (!handle) return null;
  handle.revoked = true;
  try {
    handle.fence?.();
  } catch (error) {
    // A transport callback must not prevent unregistering a revoked handle.
    diag("worker.revoke_fence_failed", {
      worker_fp: workerFp,
      error: String(error),
    });
  } finally {
    if (connectWorkers.get(workerFp) === handle) connectWorkers.delete(workerFp);
  }
  return handle;
}

/** Fingerprints the coordinator can route to right now. Raw WebSocket
 * membership alone is insufficient: the current generation must have crossed
 * its durable exact-snapshot barrier and must not be revoked. */
export function listRoutableFps(dashboardId?: string): string[] {
  if (dashboardId === undefined) {
    return [...connectWorkers.values()]
      .filter((handle) => handle.ready && !handle.revoked)
      .map((handle) => handle.workerFp);
  }
  return [...connectWorkers.values()]
    .filter((handle) =>
      handle.ready
      && !handle.revoked
      && handle.dashboardId === dashboardId
    )
    .map((handle) => handle.workerFp);
}

// Broadcast the live routable set on every connect/disconnect so the SPA's
// online indicator tracks coord's actual WS membership instead of a stale
// workersList snapshot. Full-set (replace) semantics — no per-fp fold.
export function _publishRoutable(): void {
  workerRoutableBus.publish({ fps: listRoutableFps() });
}
