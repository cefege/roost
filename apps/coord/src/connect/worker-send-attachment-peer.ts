// Sends typed attachment-peer controls to one exact authenticated worker
// generation. AttachmentPeerNegotiations installs its correlation waiter before
// an offer is sent; this module serializes only the downstream control frame.
// It never logs SDP, ICE candidates, grant material, or attachment bytes.

import { create } from "@bufbuild/protobuf";
import {
  CoordWorkerDownSchema,
  DLocalAttachmentPeerCancelSchema,
  DLocalAttachmentPeerOfferSchema,
} from "@roost/protocol/proto/worker_transport_pb";
import { connectWorkers, type WorkerHandle } from "./worker-registry.ts";
import {
  ATTACHMENT_TRANSFER_PEER_ERROR_REASONS,
  type AttachmentTransferPeerErrorReason,
} from "@roost/protocol/attachment-transfer";

export type AttachmentPeerWorkerErrorReason = AttachmentTransferPeerErrorReason;

export interface AttachmentPeerOfferSend {
  readonly requestId: string;
  readonly grantId: string;
  readonly peerId: string;
  readonly deviceFingerprint: string;
  readonly tabId: string;
  readonly workerEpoch: string;
  readonly offerSdp: string;
  readonly budgetMs: number;
  readonly stunUrls: readonly string[];
}

export interface AttachmentPeerCancelSend {
  readonly requestId: string;
  readonly peerId: string;
  readonly workerEpoch: string;
}

/** True only while this exact handle remains the worker's routable generation. */
export function isCurrentAttachmentPeerWorker(
  worker: WorkerHandle,
  expectedEpoch: string,
): boolean {
  return (
    worker.ready
    && !worker.revoked
    && worker.processEpoch === expectedEpoch
    && connectWorkers.get(worker.workerFp) === worker
  );
}

/** Sends one offer after its typed waiter has been installed by the caller. */
export function sendAttachmentPeerOffer(
  worker: WorkerHandle,
  message: AttachmentPeerOfferSend,
): boolean {
  if (!isCurrentAttachmentPeerWorker(worker, message.workerEpoch)) return false;
  try {
    return worker.send(create(CoordWorkerDownSchema, {
      frame: {
        case: "localAttachmentPeerOffer",
        value: create(DLocalAttachmentPeerOfferSchema, {
          requestId: message.requestId,
          connectionGeneration: worker.connectionGeneration,
          workerEpoch: message.workerEpoch,
          grantId: message.grantId,
          peerId: message.peerId,
          deviceFingerprint: message.deviceFingerprint,
          tabId: message.tabId,
          offerSdp: message.offerSdp,
          budgetMs: message.budgetMs,
          stunUrls: [...message.stunUrls],
        }),
      },
    })) !== 0;
  } catch {
    return false;
  }
}

/** Cancellation is safe only while the captured source generation is current. */
export function sendAttachmentPeerCancel(
  worker: WorkerHandle,
  message: AttachmentPeerCancelSend,
): boolean {
  if (!isCurrentAttachmentPeerWorker(worker, message.workerEpoch)) return false;
  try {
    return worker.send(create(CoordWorkerDownSchema, {
      frame: {
        case: "localAttachmentPeerCancel",
        value: create(DLocalAttachmentPeerCancelSchema, {
          requestId: message.requestId,
          connectionGeneration: worker.connectionGeneration,
          workerEpoch: message.workerEpoch,
          peerId: message.peerId,
        }),
      },
    })) !== 0;
  } catch {
    return false;
  }
}

export function isAttachmentPeerWorkerErrorReason(
  value: string,
): value is AttachmentPeerWorkerErrorReason {
  return (ATTACHMENT_TRANSFER_PEER_ERROR_REASONS as readonly string[]).includes(value);
}
