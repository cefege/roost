// Immutable attachment-upload metadata and dedicated-grant hello admission.
// Grant expiry gates a fresh hello only; an admitted port uses its finite active
// lease while explicit grant replacement or revocation remains an immediate fence.

import type { AttachmentTransferHello } from "@roost/shared/proto/attachment_transfer_pb";
import type { AttachmentGrant, AttachmentGrantStore } from "./attachment-grants.ts";
import type { AttachmentPeerExpectedTuple } from "./attachment-peer-connection.ts";

export interface AttachmentUploadMetadata {
  readonly grantId: string;
  readonly deviceFingerprint: string;
  readonly tabId: string;
  readonly sessionId: string;
  readonly uploadId: string;
  readonly filename: string;
  readonly shortPath: boolean;
  readonly totalBytes: number;
  readonly peerId: string;
  readonly workerEpoch: string;
}

export type AttachmentHelloAdmission =
  | { readonly ok: true; readonly metadata: AttachmentUploadMetadata }
  | { readonly ok: false; readonly reason: "invalid_hello" | "grant_unavailable" };

export interface AttachmentHelloAdmissionDeps {
  readonly grants: AttachmentGrantStore;
  readonly expectedPeer: AttachmentPeerExpectedTuple | null;
  readonly workerEpoch: string;
}

export function admitAttachmentTransferHello(
  hello: AttachmentTransferHello,
  deps: AttachmentHelloAdmissionDeps,
): AttachmentHelloAdmission {
  const verdict = deps.grants.verify({
    grantId: hello.grantId,
    secret: hello.secret,
    sessionId: hello.sessionId,
    uploadId: hello.uploadId,
    filename: hello.filename,
    shortPath: hello.shortPath,
    totalBytes: hello.totalBytes,
    deviceFingerprint: hello.deviceFingerprint,
    tabId: hello.tabId,
    workerEpoch: hello.workerEpoch,
  });
  if (!verdict.ok || !matchesExpectedPeer(deps.expectedPeer, hello)) {
    return { ok: false, reason: "grant_unavailable" };
  }
  const grant = verdict.grant;
  return {
    ok: true,
    metadata: {
      grantId: grant.grantId,
      deviceFingerprint: grant.deviceFingerprint,
      tabId: grant.tabId,
      sessionId: grant.sessionId,
      uploadId: grant.uploadId,
      filename: grant.filename,
      shortPath: grant.shortPath,
      totalBytes: grant.totalBytes,
      peerId: hello.peerId,
      workerEpoch: grant.workerEpoch,
    },
  };
}

export function attachmentMetadataMatchesGrant(
  metadata: AttachmentUploadMetadata,
  grant: AttachmentGrant,
): boolean {
  return grant.grantId === metadata.grantId
    && grant.workerEpoch === metadata.workerEpoch
    && grant.deviceFingerprint === metadata.deviceFingerprint
    && grant.tabId === metadata.tabId
    && grant.sessionId === metadata.sessionId
    && grant.uploadId === metadata.uploadId
    && grant.filename === metadata.filename
    && grant.shortPath === metadata.shortPath
    && grant.totalBytes === metadata.totalBytes;
}

function matchesExpectedPeer(expected: AttachmentPeerExpectedTuple | null, hello: AttachmentTransferHello): boolean {
  if (expected === null) return hello.peerId === "";
  return expected.peerId === hello.peerId
    && expected.grantId === hello.grantId
    && expected.deviceFingerprint === hello.deviceFingerprint
    && expected.tabId === hello.tabId
    && expected.workerEpoch === hello.workerEpoch;
}

function matchesExpectedMetadataPeer(
  expected: AttachmentPeerExpectedTuple | null,
  metadata: AttachmentUploadMetadata,
): boolean {
  if (expected === null) return metadata.peerId === "";
  return expected.peerId === metadata.peerId
    && expected.grantId === metadata.grantId
    && expected.deviceFingerprint === metadata.deviceFingerprint
    && expected.tabId === metadata.tabId
    && expected.workerEpoch === metadata.workerEpoch;
}
