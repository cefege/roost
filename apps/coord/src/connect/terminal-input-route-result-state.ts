// Holds mutable-record shapes for coordinator input-route control correlation.
// TerminalInputRouteResults owns every map containing these records; this module
// contains no registry or lifecycle behavior so socket/worker teardown remains
// centralized in that composition owner.

import type { WorkerHandle } from "./worker-registry.ts";

export interface TerminalRouteControlSlot {
  readonly connectionId: string;
  readonly browserRequestId: string;
  pending: PendingTerminalRouteControl | null;
}

export interface PendingTerminalRouteControl {
  readonly kind: "claim" | "probe";
  readonly slot: TerminalRouteControlSlot;
  readonly worker: WorkerHandle;
  readonly workerFp: string;
  readonly workerEpoch: string;
  readonly connectionGeneration: string;
  readonly sessionId: string | null;
  readonly revision: bigint | null;
  outerRequestId: string | null;
}

export function terminalRouteConnectionNonceKey(
  connectionId: string,
  requestId: string,
): string {
  return JSON.stringify([connectionId, requestId]);
}
