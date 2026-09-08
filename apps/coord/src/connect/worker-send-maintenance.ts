// Sends Windows update-broker and keeper-update commands over the current
// routable worker generation. These operations preserve their pending-RPC
// correlation so transport loss rejects the exact maintenance step instead of
// being mistaken for a completed update.

import { create } from "@bufbuild/protobuf";
import { Code, ConnectError } from "@connectrpc/connect";
import { KeeperCoordinatorOpenSessionIdsSchema } from "@roost/shared/keeper-update";
import {
  CoordWorkerDownSchema,
  DUpdateBrokerSchema,
  DKeeperUpdatePrepareSchema,
} from "@roost/shared/proto/worker_transport_pb";
import {
  createPendingRpc,
  rejectPendingRpcUnavailable,
} from "../router/pending-rpcs.ts";
import { currentRoutableWorker } from "./worker-send-target.ts";

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

export function sendKeeperUpdatePreparation(workerFp: string, message: {
  journaledUpdateJson?: string;
  direction?: "source" | "target";
  maintenance: boolean;
  forceLive: boolean;
  coordinatorOpenSessionIds: readonly string[];
}, timeoutMs = 10_000): Promise<unknown> {
  const coordinatorOpenSessionIds = KeeperCoordinatorOpenSessionIdsSchema.parse(
    message.coordinatorOpenSessionIds,
  );
  const worker = currentRoutableWorker(workerFp);
  if (!worker) throw new ConnectError("worker offline", Code.Unavailable);
  const pending = createPendingRpc(timeoutMs, workerFp);
  try {
    const sent = worker.send(create(CoordWorkerDownSchema, {
      frame: {
        case: "keeperUpdatePrepare",
        value: create(DKeeperUpdatePrepareSchema, {
          requestId: pending.request_id,
          journaledUpdateJson: message.journaledUpdateJson,
          direction: message.direction ?? "",
          maintenance: message.maintenance,
          forceLive: message.forceLive,
          coordinatorOpenSessionIds: [...coordinatorOpenSessionIds],
        }),
      },
    }));
    if (sent === 0) throw new Error("worker keeper update preparation was dropped");
  } catch (error) {
    rejectPendingRpcUnavailable(
      pending.request_id,
      error instanceof Error ? error.message : String(error),
      workerFp,
    );
  }
  return pending.promise;
}
