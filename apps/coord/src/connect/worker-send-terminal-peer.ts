// Sends typed terminal-peer control frames to one exact authenticated worker
// generation. TerminalPeerNegotiations registers its waiter before calling the
// offer sender; this module only serializes and fences downstream transport.
// It never logs SDP, ICE candidates, DTLS fingerprints, or grant material.

import { create } from "@bufbuild/protobuf";
import {
  CoordWorkerDownSchema,
  DLocalTerminalPeerCancelSchema,
  DLocalTerminalPeerOfferSchema,
} from "@roost/protocol/proto/worker_transport_pb";
import { connectWorkers, type WorkerHandle } from "./worker-registry.ts";

export const TERMINAL_PEER_WORKER_ERROR_REASONS = [
  "disabled",
  "native_unavailable",
  "invalid_offer",
  "grant_unavailable",
  "capacity",
  "expired",
  "connection_superseded",
  "ice_failed",
] as const;

export type TerminalPeerWorkerErrorReason =
  (typeof TERMINAL_PEER_WORKER_ERROR_REASONS)[number];

export interface TerminalPeerOfferSend {
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

export interface TerminalPeerCancelSend {
  readonly requestId: string;
  readonly peerId: string;
  readonly workerEpoch: string;
}

/** True only while this exact handle remains the worker's routable generation. */
export function isCurrentTerminalPeerWorker(
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
export function sendTerminalPeerOffer(
  worker: WorkerHandle,
  message: TerminalPeerOfferSend,
): boolean {
  if (!isCurrentTerminalPeerWorker(worker, message.workerEpoch)) return false;
  try {
    return worker.send(create(CoordWorkerDownSchema, {
      frame: {
        case: "localTerminalPeerOffer",
        value: create(DLocalTerminalPeerOfferSchema, {
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
export function sendTerminalPeerCancel(
  worker: WorkerHandle,
  message: TerminalPeerCancelSend,
): boolean {
  if (!isCurrentTerminalPeerWorker(worker, message.workerEpoch)) return false;
  try {
    return worker.send(create(CoordWorkerDownSchema, {
      frame: {
        case: "localTerminalPeerCancel",
        value: create(DLocalTerminalPeerCancelSchema, {
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


export function isTerminalPeerWorkerErrorReason(
  value: string,
): value is TerminalPeerWorkerErrorReason {
  return (TERMINAL_PEER_WORKER_ERROR_REASONS as readonly string[]).includes(value);
}
