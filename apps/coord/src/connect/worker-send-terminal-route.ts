// Sends typed input-route and content-free probe controls to one exact worker
// connection. TerminalInputRouteResults owns correlation and browser replies;
// this module only allocates the worker-side request id and serializes frames.
// Every send uses the same monotonic hop budget as terminal input.

import { create } from "@bufbuild/protobuf";
import {
  CoordWorkerDownSchema,
  DTerminalInputRouteClaimSchema,
  DTerminalTransportProbeSchema,
  DTerminalViewSocketClosedSchema,
  type WTerminalInputRouteResult,
  type WTerminalTransportProbeResult,
} from "@roost/shared/proto/worker_transport_pb";
import { TERMINAL_INPUT_ROUTE_CAPABILITY } from "@roost/shared/terminal-peer";
import {
  createPendingRpc,
  rejectPendingRpcUnavailable,
} from "../router/pending-rpcs.ts";
import { connectWorkers, type WorkerHandle } from "./worker-registry.ts";
import {
  startHopDeadline,
  unsentTerminalWorkerRequest,
  workerBudgetMs,
  type HopDeadline,
  type TerminalWorkerRequest,
} from "./worker-send.ts";

export const TERMINAL_ROUTE_CONTROL_TIMEOUT_MS = 8_000;

export interface TerminalInputRouteClaimSend {
  readonly sessionId: string;
  readonly deviceFingerprint: string;
  readonly tabId: string;
  readonly browserConnectionId: string;
  readonly revision: bigint;
  readonly workerEpoch: string;
}

export interface TerminalTransportProbeSend {
  readonly workerEpoch: string;
}

/** Installed before transport send so a synchronous worker result cannot race
 * the typed-result owner's exact-handle correlation record. */
export interface PendingTerminalRouteWorkerRequest<T> {
  readonly requestId: string;
  readonly result: Promise<T>;
}

export type TerminalRoutePendingInstaller<T> = (
  pending: PendingTerminalRouteWorkerRequest<T>,
) => void;

/** Rechecks composition-owned cancellation after installing correlation and before send. */
export type TerminalRoutePendingVerifier = () => boolean;

/** True only while this exact handle remains the route-capable worker generation. */
export function isCurrentTerminalInputRouteWorker(
  worker: WorkerHandle,
  expectedEpoch: string,
): boolean {
  return worker.ready
    && !worker.revoked
    && worker.processEpoch === expectedEpoch
    && worker.capabilities.has(TERMINAL_INPUT_ROUTE_CAPABILITY)
    && connectWorkers.get(worker.workerFp) === worker;
}

/** Allocates the opaque coordinator request id before emitting one route claim. */
export function sendTerminalInputRouteClaimRequest(
  worker: WorkerHandle,
  message: TerminalInputRouteClaimSend,
  installPending: TerminalRoutePendingInstaller<WTerminalInputRouteResult>,
  isStillPending: TerminalRoutePendingVerifier,
  deadline: HopDeadline = startHopDeadline(TERMINAL_ROUTE_CONTROL_TIMEOUT_MS),
): TerminalWorkerRequest<WTerminalInputRouteResult> {
  if (!isCurrentTerminalInputRouteWorker(worker, message.workerEpoch)) {
    return unsentTerminalWorkerRequest("terminal input route worker is unavailable", false);
  }
  const budgetMs = workerBudgetMs(deadline);
  if (budgetMs === null) {
    return unsentTerminalWorkerRequest("terminal input route budget expired before send", true);
  }
  const pending = createPendingRpc<WTerminalInputRouteResult>(
    Math.max(1, Math.ceil(deadline.remainingMs())),
    worker.workerFp,
  );
  try {
    installPending({ requestId: pending.request_id, result: pending.promise });
  } catch (error) {
    rejectPendingRpcUnavailable(
      pending.request_id,
      error instanceof Error ? error.message : "terminal input route correlation failed",
      worker.workerFp,
    );
    return {
      admitted: false,
      expired: false,
      requestId: pending.request_id,
      result: pending.promise,
    };
  }
  if (!isStillPending() || !isCurrentTerminalInputRouteWorker(worker, message.workerEpoch)) {
    rejectPendingRpcUnavailable(
      pending.request_id,
      "terminal input route control was cancelled before send",
      worker.workerFp,
    );
    return {
      admitted: false,
      expired: false,
      requestId: pending.request_id,
      result: pending.promise,
    };
  }
  let admitted = false;
  try {
    admitted = worker.send(create(CoordWorkerDownSchema, {
      frame: {
        case: "terminalInputRouteClaim",
        value: create(DTerminalInputRouteClaimSchema, {
          requestId: pending.request_id,
          sessionId: message.sessionId,
          deviceFingerprint: message.deviceFingerprint,
          tabId: message.tabId,
          browserConnectionId: message.browserConnectionId,
          revision: message.revision,
          budgetMs,
          workerEpoch: message.workerEpoch,
        }),
      },
    })) !== 0;
    if (!admitted) {
      rejectPendingRpcUnavailable(
        pending.request_id,
        "worker transport dropped terminal input route claim",
        worker.workerFp,
      );
    }
  } catch (error) {
    rejectPendingRpcUnavailable(
      pending.request_id,
      error instanceof Error ? error.message : "worker transport failed terminal input route claim",
      worker.workerFp,
    );
  }
  return {
    admitted,
    expired: false,
    requestId: pending.request_id,
    result: pending.promise,
  };
}

/** Allocates the opaque coordinator request id before emitting one empty probe. */
export function sendTerminalTransportProbeRequest(
  worker: WorkerHandle,
  message: TerminalTransportProbeSend,
  installPending: TerminalRoutePendingInstaller<WTerminalTransportProbeResult>,
  isStillPending: TerminalRoutePendingVerifier,
  deadline: HopDeadline = startHopDeadline(TERMINAL_ROUTE_CONTROL_TIMEOUT_MS),
): TerminalWorkerRequest<WTerminalTransportProbeResult> {
  if (!isCurrentTerminalInputRouteWorker(worker, message.workerEpoch)) {
    return unsentTerminalWorkerRequest("terminal transport probe worker is unavailable", false);
  }
  const budgetMs = workerBudgetMs(deadline);
  if (budgetMs === null) {
    return unsentTerminalWorkerRequest("terminal transport probe budget expired before send", true);
  }
  const pending = createPendingRpc<WTerminalTransportProbeResult>(
    Math.max(1, Math.ceil(deadline.remainingMs())),
    worker.workerFp,
  );
  try {
    installPending({ requestId: pending.request_id, result: pending.promise });
  } catch (error) {
    rejectPendingRpcUnavailable(
      pending.request_id,
      error instanceof Error ? error.message : "terminal transport probe correlation failed",
      worker.workerFp,
    );
    return {
      admitted: false,
      expired: false,
      requestId: pending.request_id,
      result: pending.promise,
    };
  }
  if (!isStillPending() || !isCurrentTerminalInputRouteWorker(worker, message.workerEpoch)) {
    rejectPendingRpcUnavailable(
      pending.request_id,
      "terminal transport probe control was cancelled before send",
      worker.workerFp,
    );
    return {
      admitted: false,
      expired: false,
      requestId: pending.request_id,
      result: pending.promise,
    };
  }
  let admitted = false;
  try {
    admitted = worker.send(create(CoordWorkerDownSchema, {
      frame: {
        case: "terminalTransportProbe",
        value: create(DTerminalTransportProbeSchema, {
          requestId: pending.request_id,
          workerEpoch: message.workerEpoch,
        }),
      },
    })) !== 0;
    if (!admitted) {
      rejectPendingRpcUnavailable(
        pending.request_id,
        "worker transport dropped terminal transport probe",
        worker.workerFp,
      );
    }
  } catch (error) {
    rejectPendingRpcUnavailable(
      pending.request_id,
      error instanceof Error ? error.message : "worker transport failed terminal transport probe",
      worker.workerFp,
    );
  }
  return {
    admitted,
    expired: false,
    requestId: pending.request_id,
    result: pending.promise,
  };
}

/** Retires the worker's route actor for a closed Sync socket on this exact generation. */
export function sendTerminalInputRouteConnectionClosed(
  worker: WorkerHandle,
  workerEpoch: string,
  socketId: string,
): boolean {
  if (!isCurrentTerminalInputRouteWorker(worker, workerEpoch)) return false;
  try {
    return worker.send(create(CoordWorkerDownSchema, {
      frame: {
        case: "terminalViewSocketClosed",
        value: create(DTerminalViewSocketClosedSchema, { socketId }),
      },
    })) !== 0;
  } catch {
    return false;
  }
}
