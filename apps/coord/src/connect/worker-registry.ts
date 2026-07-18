// Worker-connection registry: the shared Map of currently-attached workers
// keyed by fingerprint, plus the routable-set accessors the router and the
// SPA online indicator read. Populated/cleared by makeWorkerConn
// (worker-conn.ts) as raw WSs attach/close.

import type { CoordWorkerDown } from "@roost/shared/proto/worker_transport_pb";
import { workerRoutableBus } from "../buses.ts";

export interface WorkerHandle {
  workerFp: string;
  send: (frame: CoordWorkerDown) => void;
}

// Registry of currently-attached workers, keyed by fp. Populated when a
// worker's raw WS upgrades (worker-ws-handler.ts), removed on close. The
// authoritative "is this worker routable" fact — heartbeat freshness is a
// SEPARATE, weaker signal (see workersList routable field).
export const connectWorkers = new Map<string, WorkerHandle>();

/** Test-only seam: install/remove a fake WorkerHandle without going
 *  through the full raw-WS attach handshake. Used by
 *  coord-bidi.test.ts to drive `_forwardSimple` → `_bumpViewer` for
 *  multi-viewer / sessionsResize assertions where spinning up a real
 *  worker WS is out of scope. Production code paths never call this. */
export function __setConnectWorkerForTest(workerFp: string, handle: WorkerHandle | null): void {
  if (handle) connectWorkers.set(workerFp, handle);
  else connectWorkers.delete(workerFp);
}

/** A2: fps the coord can route to right now (raw-WS membership). The
 *  authoritative "usable" set — distinct from last_seen_ms heartbeat
 *  freshness (a worker can heartbeat over the unary transport while its WS
 *  is down). workersList exposes this so the SPA online indicator doesn't
 *  lie. */
export function listRoutableFps(): string[] {
  return [...connectWorkers.keys()];
}

// Broadcast the live routable set on every connect/disconnect so the SPA's
// online indicator tracks coord's actual WS membership instead of a stale
// workersList snapshot. Full-set (replace) semantics — no per-fp fold.
export function _publishRoutable(): void {
  workerRoutableBus.publish({ fps: listRoutableFps() });
}
