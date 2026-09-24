// Sends separate attachment-grant controls to one exact worker generation. The
// grant owner awaits the worker acknowledgement before exposing a browser secret.
// This path receives only a secret digest and immutable upload descriptor, never
// attachment bytes or raw credentials, and it does not share terminal grant state.

import { create } from "@bufbuild/protobuf";
import { Code, ConnectError } from "@connectrpc/connect";
import {
  CoordWorkerDownSchema,
  DLocalAttachmentGrantRevokeSchema,
  DLocalAttachmentGrantSchema,
} from "@roost/protocol/proto/worker_transport_pb";
import { ATTACHMENT_TRANSFER_GRANT_ACK_DEADLINE_MS } from "@roost/protocol/attachment-transfer";
import {
  createPendingRpc,
  rejectPendingRpcUnavailable,
} from "../router/pending-rpcs.ts";
import type { AttachmentGrantDescriptor } from "./attachment-grant-owner-state.ts";
import { isExactAttachmentGrantWorker } from "./attachment-grant-owner-state.ts";
import type { WorkerHandle } from "../workers/worker-registry.ts";

export interface LocalAttachmentGrantInstall {
  readonly grantId: string;
  readonly secretSha256: string;
  readonly descriptor: AttachmentGrantDescriptor;
  readonly deviceFingerprint: string;
  readonly tabId: string;
  readonly ttlMs: number;
}

export interface PendingLocalAttachmentGrantInstall {
  readonly requestId: string;
  readonly promise: Promise<unknown>;
}

/** Installs one immutable attachment grant on the captured worker generation. */
export function sendLocalAttachmentGrantRequest(
  worker: WorkerHandle,
  workerEpoch: string,
  message: LocalAttachmentGrantInstall,
  timeoutMs = ATTACHMENT_TRANSFER_GRANT_ACK_DEADLINE_MS,
): PendingLocalAttachmentGrantInstall {
  if (!isExactAttachmentGrantWorker(worker, workerEpoch)) {
    throw new ConnectError("worker offline", Code.Unavailable);
  }
  const pending = createPendingRpc(timeoutMs, worker.workerFp);
  try {
    const sent = worker.send(create(CoordWorkerDownSchema, {
      frame: {
        case: "localAttachmentGrant",
        value: create(DLocalAttachmentGrantSchema, {
          requestId: pending.request_id,
          grantId: message.grantId,
          secretSha256: message.secretSha256,
          sessionId: message.descriptor.sessionId,
          uploadId: message.descriptor.uploadId,
          filename: message.descriptor.filename,
          shortPath: message.descriptor.shortPath,
          totalBytes: BigInt(message.descriptor.totalBytes),
          deviceFingerprint: message.deviceFingerprint,
          tabId: message.tabId,
          ttlMs: message.ttlMs,
          workerEpoch,
        }),
      },
    }));
    if (sent === 0) throw new Error("worker dropped the attachment grant");
  } catch {
    rejectPendingRpcUnavailable(pending.request_id, "worker transport unavailable", worker.workerFp);
  }
  return { requestId: pending.request_id, promise: pending.promise };
}

/** Device revocation clears every attachment credential held by this worker. */
export function sendLocalAttachmentGrantRevoke(
  worker: WorkerHandle,
  deviceFingerprint: string,
): boolean {
  if (worker.processEpoch === null || !isExactAttachmentGrantWorker(worker, worker.processEpoch)) return false;
  try {
    return worker.send(create(CoordWorkerDownSchema, {
      frame: {
        case: "localAttachmentGrantRevoke",
        value: create(DLocalAttachmentGrantRevokeSchema, { deviceFingerprint }),
      },
    })) !== 0;
  } catch {
    return false;
  }
}
