// Defines the separate attachment-grant authority's immutable lease records and
// failure vocabulary. AttachmentGrantOwner owns mutations and worker controls;
// this module holds only data shapes and exact-worker predicates. Attachment
// grants never share a registry, secret, or lease identity with terminal grants.

import { Code, ConnectError } from "@connectrpc/connect";
import type { WorkerHandle } from "../workers/worker-registry.ts";
import { currentRoutableWorker } from "../workers/worker-send-target.ts";
import { hasAtMostUtf8Bytes } from "@roost/protocol/ui-state";

export const ATTACHMENT_GRANT_MAX_IDENTIFIER_UTF8_BYTES = 128;
export const ATTACHMENT_GRANT_MAX_FILENAME_UTF8_BYTES = 1024;

export type AttachmentGrantRetireReason = "worker_deleted" | "worker_revoked";
export type AttachmentGrantInvalidationKind = "grant_expired" | "device_revoked"
  | "worker_retired" | "disposed";

export interface AttachmentGrantDescriptor {
  readonly sessionId: string;
  readonly uploadId: string;
  readonly filename: string;
  readonly shortPath: boolean;
  readonly totalBytes: number;
}

export interface AttachmentGrantLeaseSnapshot {
  readonly grantId: string;
  readonly ownerKey: string;
  readonly deviceFingerprint: string;
  readonly tabId: string;
  readonly workerFp: string;
  readonly workerEpoch: string;
  readonly descriptor: AttachmentGrantDescriptor;
  readonly expiresAtMs: number;
  readonly workerHandle: WorkerHandle;
}

export interface AttachmentGrantLease extends AttachmentGrantLeaseSnapshot {
  timer: NodeJS.Timeout | undefined;
}

export interface AttachmentGrantInvalidation {
  readonly kind: AttachmentGrantInvalidationKind;
  readonly lease: AttachmentGrantLeaseSnapshot | null;
  readonly workerFp: string;
  readonly workerEpoch: string | null;
  readonly deviceFingerprint: string | null;
  readonly reason: AttachmentGrantRetireReason | null;
}

export interface AttachmentGrantPort {
  ownedGrant(
    ownerKey: string,
    tabId: string,
    workerFp: string,
    grantId: string,
  ): AttachmentGrantLeaseSnapshot | null;
  subscribeInvalidation(listener: (invalidation: AttachmentGrantInvalidation) => void): () => void;
}

export type AttachmentGrantAuthorization = () => Promise<void>;

export interface AttachmentGrantRequest {
  readonly ownerKey: string;
  readonly deviceFingerprint: string;
  readonly tabId: string;
  readonly workerFp: string;
  readonly descriptor: AttachmentGrantDescriptor;
  readonly authorize: AttachmentGrantAuthorization;
}

export interface AttachmentGrantResult {
  readonly lease: AttachmentGrantLeaseSnapshot;
  readonly secret: string;
}

export interface PendingAttachmentGrant {
  readonly grantId: string;
  readonly deviceFingerprint: string;
  readonly workerFp: string;
  invalidated: boolean;
}

export function attachmentGrantSnapshot(
  lease: AttachmentGrantLeaseSnapshot,
): AttachmentGrantLeaseSnapshot {
  return {
    grantId: lease.grantId,
    ownerKey: lease.ownerKey,
    deviceFingerprint: lease.deviceFingerprint,
    tabId: lease.tabId,
    workerFp: lease.workerFp,
    workerEpoch: lease.workerEpoch,
    descriptor: { ...lease.descriptor },
    expiresAtMs: lease.expiresAtMs,
    workerHandle: lease.workerHandle,
  };
}

export function isExactAttachmentGrantWorker(
  worker: WorkerHandle,
  workerEpoch: string,
): boolean {
  return worker.processEpoch === workerEpoch
    && currentRoutableWorker(worker.workerFp) === worker;
}

export function assertAttachmentGrantRequest(request: AttachmentGrantRequest): void {
  for (const [field, value] of [
    ["owner", request.ownerKey],
    ["device", request.deviceFingerprint],
    ["tab", request.tabId],
    ["worker", request.workerFp],
    ["session", request.descriptor.sessionId],
  ] as const) {
    if (value.length === 0 || !hasAtMostUtf8Bytes(value, ATTACHMENT_GRANT_MAX_IDENTIFIER_UTF8_BYTES)) {
      throw attachmentGrantInvalid(`attachment grant ${field} is invalid`);
    }
  }
  const { uploadId, filename, totalBytes } = request.descriptor;
  if (
    uploadId.length === 0
    || !hasAtMostUtf8Bytes(uploadId, ATTACHMENT_GRANT_MAX_IDENTIFIER_UTF8_BYTES)
    || /[\\/\x00-\x1f\x7f]/u.test(uploadId)
    || filename.length === 0
    || !hasAtMostUtf8Bytes(filename, ATTACHMENT_GRANT_MAX_FILENAME_UTF8_BYTES)
    || !Number.isSafeInteger(totalBytes)
    || totalBytes < 0
  ) throw attachmentGrantInvalid("attachment grant descriptor is invalid");
}

export function attachmentGrantInvalid(message: string): ConnectError {
  return new ConnectError(message, Code.InvalidArgument);
}

export function attachmentGrantDenied(message: string): ConnectError {
  return new ConnectError(message, Code.PermissionDenied);
}

export function attachmentGrantUnavailable(message: string): ConnectError {
  return new ConnectError(message, Code.Unavailable);
}

export function attachmentGrantExhausted(message: string): ConnectError {
  return new ConnectError(message, Code.ResourceExhausted);
}
