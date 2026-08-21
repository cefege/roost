// Bounded diagnostic snapshot requests over live worker connections.

import { create } from "@bufbuild/protobuf";
import { Code, ConnectError } from "@connectrpc/connect";
import {
  CoordWorkerDownSchema,
  DBrowserCommandSchema,
} from "@roost/shared/proto/worker_transport_pb";
import { createPendingRpc, rejectPendingRpcUnavailable } from "../router/pending-rpcs.ts";
import { connectWorkers } from "./worker-registry.ts";

export type WorkerDiagSnapshotErrorCode =
  | "offline"
  | "timeout"
  | "send_failed"
  | "rpc_error";

export type WorkerDiagSnapshotResult =
  | {
      status: "ok";
      response_ms: number;
      snapshot: Record<string, unknown>;
    }
  | {
      status: "error";
      response_ms: number;
      error: {
        code: WorkerDiagSnapshotErrorCode;
        message: string;
      };
    };

const DIAG_SNAPSHOT_TIMEOUT_MS = 2_000;

function diagError(
  startedAtMs: number,
  code: WorkerDiagSnapshotErrorCode,
  message: string,
): WorkerDiagSnapshotResult {
  return {
    status: "error",
    response_ms: Math.max(0, Date.now() - startedAtMs),
    error: { code, message: message.slice(0, 240) },
  };
}

async function requestWorkerDiagSnapshot(
  workerFp: string,
  timeoutMs: number,
): Promise<WorkerDiagSnapshotResult> {
  const startedAtMs = Date.now();
  const worker = connectWorkers.get(workerFp);
  if (!worker) return diagError(startedAtMs, "offline", "worker is not connected");

  const pending = createPendingRpc<Record<string, unknown>>(timeoutMs, workerFp);
  try {
    const sent = worker.send(create(CoordWorkerDownSchema, {
      frame: { case: "browserCommand", value: create(DBrowserCommandSchema, {
        browserId: "coordinator-diag",
        viewerId: "coordinator-diag",
        requestId: pending.request_id,
        frameJson: JSON.stringify({ kind: "diag-snapshot" }),
      }) },
    }));
    if (sent === 0) {
      rejectPendingRpcUnavailable(
        pending.request_id,
        "worker transport dropped diagnostic snapshot request",
      );
      await pending.promise.catch(() => undefined);
      return diagError(startedAtMs, "send_failed", "worker transport dropped request");
    }
  } catch (error) {
    rejectPendingRpcUnavailable(
      pending.request_id,
      error instanceof Error ? error.message : "worker transport failed diagnostic snapshot",
    );
    await pending.promise.catch(() => undefined);
    return diagError(
      startedAtMs,
      "send_failed",
      error instanceof Error ? error.message : "worker transport send failed",
    );
  }

  try {
    const snapshot = await pending.promise;
    if (!snapshot || typeof snapshot !== "object" || Array.isArray(snapshot)) {
      return diagError(startedAtMs, "rpc_error", "worker returned an invalid diagnostic snapshot");
    }
    return {
      status: "ok",
      response_ms: Math.max(0, Date.now() - startedAtMs),
      snapshot,
    };
  } catch (error) {
    const code = error instanceof ConnectError
      ? error.code === Code.DeadlineExceeded
        ? "timeout"
        : error.code === Code.Unavailable
          ? "offline"
          : "rpc_error"
      : "rpc_error";
    return diagError(
      startedAtMs,
      code,
      error instanceof Error ? error.message : String(error),
    );
  }
}

/** Fan out one bounded, correlated request per known worker. The registry key
 * is the authenticated fingerprint from the worker hello; payload identity is
 * never used as a result key. Promise.allSettled isolates worker failures, and
 * each pending RPC owns a deadline/cleanup timer. */
export async function collectWorkerDiagSnapshots(
  workerFps: Iterable<string> = connectWorkers.keys(),
  timeoutMs = DIAG_SNAPSHOT_TIMEOUT_MS,
): Promise<Record<string, WorkerDiagSnapshotResult>> {
  const boundedTimeoutMs = Number.isFinite(timeoutMs)
    ? Math.max(1, Math.min(timeoutMs, 10_000))
    : DIAG_SNAPSHOT_TIMEOUT_MS;
  const fingerprints = [...new Set(workerFps)].sort();
  const startedAtMs = Date.now();
  const settled = await Promise.allSettled(
    fingerprints.map((workerFp) =>
      requestWorkerDiagSnapshot(workerFp, boundedTimeoutMs)),
  );
  const entries = fingerprints.map((workerFp, index) => {
    const result = settled[index]!;
    return [
      workerFp,
      result.status === "fulfilled"
        ? result.value
        : diagError(
            startedAtMs,
            "rpc_error",
            result.reason instanceof Error ? result.reason.message : String(result.reason),
          ),
    ] as const;
  });
  return Object.fromEntries(entries);
}
