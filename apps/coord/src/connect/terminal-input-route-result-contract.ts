// Defines bounded route-control requests and typed worker-result validation.
// TerminalInputRouteResults owns state and lifecycle; callers share these exact
// limits so a Sync admission cannot disagree with typed-result correlation.
// No worker frame is sent from this pure contract module.

import { hasAtMostUtf8Bytes } from "@roost/protocol/ui-state";
import { TERMINAL_PEER_ROUTE_CLAIM_MAX_OUTSTANDING } from "@roost/protocol/terminal-peer";
import type {
  WTerminalInputRouteResult,
  WTerminalTransportProbeResult,
} from "@roost/protocol/proto/worker_transport_pb";
import type { WorkerHandle } from "./worker-registry.ts";
import type { HopDeadline } from "./worker-send.ts";
import type { PendingTerminalRouteControl } from "./terminal-input-route-result-state.ts";

export const MAX_TERMINAL_ROUTE_CONTROLS_PER_SOCKET =
  TERMINAL_PEER_ROUTE_CLAIM_MAX_OUTSTANDING;
export const TERMINAL_ROUTE_IDENTIFIER_MAX_UTF8_BYTES = 128;
export const MAX_TERMINAL_INPUT_ROUTE_REVISION = (1n << 63n) - 1n;

export type TerminalRouteControlRefusalReason =
  | "route_claim_busy"
  | "terminal_input_route_unavailable"
  | "terminal_transport_probe_unavailable";

/** A definite refusal means no route-control frame reached the worker. */
export class TerminalRouteControlRefusal extends Error {
  constructor(readonly reason: TerminalRouteControlRefusalReason) {
    super(reason);
  }
}

export interface TerminalInputRouteClaimRequest {
  readonly browserRequestId: string;
  readonly sessionId: string;
  readonly revision: bigint;
  readonly deviceFingerprint: string;
  readonly tabId: string;
  readonly connectionId: string;
  readonly worker: WorkerHandle;
  readonly workerEpoch: string;
  /** Tests may supply a deterministic monotonic budget. Production omits it. */
  readonly deadline?: HopDeadline;
}

export interface TerminalTransportProbeRequest {
  readonly browserRequestId: string;
  readonly connectionId: string;
  readonly workerFp: string;
  readonly worker: WorkerHandle;
  readonly workerEpoch: string;
  /** Tests may supply a deterministic monotonic budget. Production omits it. */
  readonly deadline?: HopDeadline;
}

export function isTerminalRouteIdentifier(value: string): boolean {
  return value.length > 0
    && hasAtMostUtf8Bytes(value, TERMINAL_ROUTE_IDENTIFIER_MAX_UTF8_BYTES);
}

export function isValidTerminalInputRouteResult(
  pending: PendingTerminalRouteControl,
  frame: WTerminalInputRouteResult,
): boolean {
  const result = frame.result;
  if (
    !result
    || pending.outerRequestId === null
    || frame.requestId !== pending.outerRequestId
    || result.requestId !== pending.outerRequestId
    || result.sessionId !== pending.sessionId
    || result.revision !== pending.revision
    || result.workerEpoch !== pending.workerEpoch
    || result.latestRevision < 0n
    || result.latestRevision > MAX_TERMINAL_INPUT_ROUTE_REVISION
    || !hasAtMostUtf8Bytes(result.reason, TERMINAL_ROUTE_IDENTIFIER_MAX_UTF8_BYTES)
  ) return false;
  if (result.accepted) return isTerminalRouteIdentifier(result.inputRouteEpoch);
  return result.inputRouteEpoch === "";
}

export function isValidTerminalTransportProbeResult(
  pending: PendingTerminalRouteControl,
  frame: WTerminalTransportProbeResult,
): boolean {
  return pending.outerRequestId !== null
    && frame.requestId === pending.outerRequestId
    && frame.workerEpoch === pending.workerEpoch;
}
