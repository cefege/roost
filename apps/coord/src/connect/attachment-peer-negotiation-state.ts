// Defines pending attachment-peer records and admission vocabulary without
// retaining mutable registry state. AttachmentPeerNegotiations owns lifecycle
// and correlation; this module never retains SDP or other credential material.
// Direct attachment signaling reuses only the authoritative grant lease tuple.

import { Code, ConnectError } from "@connectrpc/connect";
import type {
  SessionsNegotiateAttachmentPeerRequest,
  SessionsNegotiateAttachmentPeerResponse,
} from "@roost/shared/proto/coordinator_pb";
import type {
  WLocalAttachmentPeerAnswer,
  WLocalAttachmentPeerError,
} from "@roost/shared/proto/worker_transport_pb";
import {
  ATTACHMENT_TRANSFER_PEER_MAX_NEGOTIATIONS_PER_WORKER,
  ATTACHMENT_TRANSFER_PEER_MAX_PENDING_NEGOTIATIONS,
  ATTACHMENT_TRANSFER_PEER_MAX_PENDING_NEGOTIATIONS_PER_DEVICE,
} from "@roost/shared/attachment-transfer";
import { TERMINAL_PEER_SDP_MAX_UTF8_BYTES } from "@roost/shared/terminal-peer";
import { hasAtMostUtf8Bytes } from "@roost/shared/ui-state";
import type { CoordConfig } from "@roost/shared/config";
import type {
  AttachmentGrantInvalidation,
  AttachmentGrantPort,
} from "./attachment-grant-owner-state.ts";
import type { WorkerHandle } from "./worker-registry.ts";

export const ATTACHMENT_PEER_MAX_IDENTIFIER_UTF8_BYTES = 128;
const ATTACHMENT_PEER_UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/iu;
export type AttachmentPeerNegotiationTimer = NodeJS.Timeout;

export type { AttachmentGrantInvalidation };

export interface AttachmentPeerNegotiationClock {
  now(): number;
  setTimeout(callback: () => void, delayMs: number): AttachmentPeerNegotiationTimer;
  clearTimeout(timer: AttachmentPeerNegotiationTimer): void;
}

export interface AttachmentPeerNegotiationsOptions {
  readonly cfg: Pick<CoordConfig, "terminalPeerEnabled" | "terminalPeerStunUrls">;
  readonly attachmentGrants: AttachmentGrantPort;
  readonly currentWorker?: (workerFp: string) => WorkerHandle | null;
  readonly clock?: AttachmentPeerNegotiationClock;
  readonly createRequestId?: () => string;
  readonly answerTimeoutMs?: number;
}

export interface AttachmentPeerNegotiationWorkerResultSink {
  acceptAnswer(source: WorkerHandle, answer: WLocalAttachmentPeerAnswer): boolean;
  acceptError(source: WorkerHandle, error: WLocalAttachmentPeerError): boolean;
  cancelForWorkerHandle(worker: WorkerHandle, reason: string): void;
}

export interface PendingAttachmentPeerNegotiation {
  readonly requestId: string;
  readonly ownerKey: string;
  readonly deviceFingerprint: string;
  readonly tabId: string;
  readonly workerFp: string;
  readonly grantId: string;
  readonly peerId: string;
  readonly worker: WorkerHandle;
  readonly connectionGeneration: string;
  readonly workerEpoch: string;
  readonly offerDigest: string;
  readonly deadlineAtMono: number;
  readonly signal: AbortSignal;
  readonly promise: Promise<SessionsNegotiateAttachmentPeerResponse>;
  readonly resolve: (response: SessionsNegotiateAttachmentPeerResponse) => void;
  readonly reject: (error: Error) => void;
  abortListener: () => void;
  timer: AttachmentPeerNegotiationTimer | null;
}

export const realAttachmentPeerNegotiationClock: AttachmentPeerNegotiationClock = {
  now: () => performance.now(),
  setTimeout: (callback, delayMs) => setTimeout(callback, delayMs),
  clearTimeout: (timer) => clearTimeout(timer),
};

export function assertAttachmentPeerRequestShape(
  request: SessionsNegotiateAttachmentPeerRequest,
): void {
  for (const [field, value] of [
    ["worker_fp", request.workerFp],
    ["grant_id", request.grantId],
    ["tab_id", request.tabId],
    ["peer_id", request.peerId],
    ["worker_epoch", request.workerEpoch],
  ] as const) {
    if (value.length === 0 || !hasAtMostUtf8Bytes(value, ATTACHMENT_PEER_MAX_IDENTIFIER_UTF8_BYTES)) {
      throw attachmentPeerInvalid(`attachment peer ${field} is invalid`);
    }
  }
  if (!ATTACHMENT_PEER_UUID.test(request.peerId)) {
    throw attachmentPeerInvalid("attachment peer peer_id is invalid");
  }
  if (!hasAtMostUtf8Bytes(request.offerSdp, TERMINAL_PEER_SDP_MAX_UTF8_BYTES)) {
    throw attachmentPeerInvalid("attachment peer offer is invalid");
  }
}


export function attachmentPeerKey(ownerKey: string, tabId: string, workerFp: string): string {
  return JSON.stringify([ownerKey, tabId, workerFp]);
}

export function attachmentPeerInvalid(message: string): ConnectError {
  return new ConnectError(message, Code.InvalidArgument);
}

export function attachmentPeerDenied(message: string): ConnectError {
  return new ConnectError(message, Code.PermissionDenied);
}

export function attachmentPeerUnavailable(message: string): ConnectError {
  return new ConnectError(message, Code.Unavailable);
}

export function attachmentPeerExhausted(message: string): ConnectError {
  return new ConnectError(message, Code.ResourceExhausted);
}

export function attachmentPeerAlreadyExists(message: string): ConnectError {
  return new ConnectError(message, Code.AlreadyExists);
}

export function attachmentPeerCancelled(message: string): ConnectError {
  return new ConnectError(message, Code.Canceled);
}

export function attachmentPeerDeadlineExceeded(message: string): ConnectError {
  return new ConnectError(message, Code.DeadlineExceeded);
}

export function attachmentPeerWorkerFailure(reason: string): Error {
  switch (reason) {
    case "grant_unavailable":
    case "expired":
      return attachmentPeerDenied("attachment peer worker rejected the grant");
    case "capacity":
      return attachmentPeerExhausted("attachment peer worker capacity is exhausted");
    default:
      return attachmentPeerUnavailable("attachment peer worker is unavailable");
  }
}

export function incrementAttachmentPeerCount(counts: Map<string, number>, key: string): void {
  counts.set(key, (counts.get(key) ?? 0) + 1);
}

export function decrementAttachmentPeerCount(counts: Map<string, number>, key: string): void {
  const count = counts.get(key);
  if (count === undefined || count <= 1) counts.delete(key);
  else counts.set(key, count - 1);
}

