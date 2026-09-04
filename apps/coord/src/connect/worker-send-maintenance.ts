// Sends coordinator relocation snapshots and Windows update-broker commands
// over the current routable worker generation. These operations preserve their
// pending-RPC correlation so transport loss rejects the exact maintenance step
// instead of being mistaken for a completed handoff or update.

import { create } from "@bufbuild/protobuf";
import { Code, ConnectError } from "@connectrpc/connect";
import {
  CoordWorkerDownSchema,
  DCoordMovePrepareSchema,
  DCoordMoveSnapshotStartSchema,
  DCoordMoveSnapshotChunkSchema,
  DCoordRelocateSchema,
  DUpdateBrokerSchema,
} from "@roost/shared/proto/worker_transport_pb";
import {
  createPendingRpc,
  rejectPendingRpcUnavailable,
} from "../router/pending-rpcs.ts";
import { currentRoutableWorker } from "./worker-send-target.ts";

export async function sendCoordinatorMovePrepare(workerFp: string, message: {
  handoffId: string; sourceUrl: string; targetUrl: string; expectedCoordKid: string;
  expectedGitSha: string; estimatedDbSize: bigint; action: "CHECK" | "PREPARE";
}, timeoutMs = 180_000): Promise<unknown> {
  const worker = currentRoutableWorker(workerFp);
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
  const worker = currentRoutableWorker(workerFp);
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
  const worker = currentRoutableWorker(workerFp);
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
  const worker = currentRoutableWorker(workerFp);
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
  const worker = currentRoutableWorker(workerFp);
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
    rejectPendingRpcUnavailable(
      pending.request_id,
      (error as Error).message,
      workerFp,
    );
  }
  return { requestId: pending.request_id, promise: pending.promise };
}
