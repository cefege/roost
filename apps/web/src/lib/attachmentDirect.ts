// Selects one attachment carrier before sequence zero for one browser File upload.
// It mints a separate exact attachment grant, then tries matching loopback before WebRTC.
// After bytes leave either direct route, only that route's status control may settle it.

import { ATTACHMENT_TRANSFER_STATUS_DEADLINE_MS } from "@roost/shared/attachment-transfer";
import { coordClient } from "../connect.ts";
import { readLocalWorkerDoor, type LocalWorkerDoor } from "./localWorkerDiscovery.ts";
import {
  mintAttachmentDirectGrant,
  type AttachmentDirectGrant,
  type AttachmentDirectGrantRequest,
} from "./attachmentDirectGrant.ts";
import {
  openAttachmentLoopbackTransfer,
  type AttachmentLoopbackTransferOptions,
} from "../ws/attachment-loopback.ts";
import {
  openAttachmentPeerTransfer,
  type AttachmentPeerTransferOptions,
} from "./attachmentPeer.ts";
import {
  AttachmentTransferCarrierError,
  sendAttachmentFile,
  type AttachmentTransferConnection,
  type AttachmentTransferResult,
  type AttachmentTransferStatus,
} from "./attachmentTransfer.ts";

export interface AttachmentDirectUploadRequest {
  readonly workerFp: string | undefined;
  readonly sessionId: string;
  readonly uploadId: string;
  readonly file: File;
  readonly shortPath: boolean;
  readonly onProgress?: (bytesSent: number) => void;
}

export interface AttachmentDirectDependencies {
  readonly readLocalWorkerDoor: () => LocalWorkerDoor | null;
  readonly mintGrant: (request: AttachmentDirectGrantRequest) => Promise<AttachmentDirectGrant | null>;
  readonly openLoopback: (options: AttachmentLoopbackTransferOptions) => Promise<AttachmentTransferConnection>;
  readonly openPeer: (options: AttachmentPeerTransferOptions) => Promise<AttachmentTransferConnection>;
  readonly peerAvailable: () => boolean;
  readonly createPeerId: () => string;
  readonly readCoordinatorStatus: (sessionId: string, uploadId: string) => Promise<AttachmentTransferStatus>;
}

const defaultDependencies: AttachmentDirectDependencies = {
  readLocalWorkerDoor,
  mintGrant: mintAttachmentDirectGrant,
  openLoopback: openAttachmentLoopbackTransfer,
  openPeer: openAttachmentPeerTransfer,
  peerAvailable: () => globalThis.isSecureContext === true && typeof RTCPeerConnection !== "undefined",
  createPeerId: () => crypto.randomUUID(),
  readCoordinatorStatus,
};

/** Returns null only when no direct carrier accepted sequence zero. */
export async function uploadAttachmentDirect(
  request: AttachmentDirectUploadRequest,
  suppliedDependencies: Partial<AttachmentDirectDependencies> = {},
): Promise<AttachmentTransferResult | null> {
  const dependencies = { ...defaultDependencies, ...suppliedDependencies };
  const workerFp = request.workerFp;
  if (!workerFp || !Number.isSafeInteger(request.file.size) || request.file.size < 0) return null;
  const door = dependencies.readLocalWorkerDoor();
  const peerAvailable = dependencies.peerAvailable();
  if (door?.workerFingerprint !== workerFp && !peerAvailable) return null;
  const grant = await dependencies.mintGrant({
    workerFp,
    sessionId: request.sessionId,
    uploadId: request.uploadId,
    filename: request.file.name,
    shortPath: request.shortPath,
    totalBytes: request.file.size,
  });
  if (!validGrant(grant, request, workerFp)) return null;
  if (door?.workerFingerprint === workerFp) {
    const result = await tryDirectCarrier(
      () => dependencies.openLoopback({ door, grant }),
      request,
      dependencies.readCoordinatorStatus,
    );
    if (result) return result;
  }
  if (!grant.peerSupported || !peerAvailable) return null;
  let peerId: string;
  try {
    peerId = dependencies.createPeerId();
  } catch {
    return null;
  }
  return tryDirectCarrier(
    () => dependencies.openPeer({ grant, peerId }),
    request,
    dependencies.readCoordinatorStatus,
  );
}

async function tryDirectCarrier(
  open: () => Promise<AttachmentTransferConnection>,
  request: AttachmentDirectUploadRequest,
  readCoordinatorStatus: AttachmentDirectDependencies["readCoordinatorStatus"],
): Promise<AttachmentTransferResult | null> {
  let connection: AttachmentTransferConnection | null = null;
  try {
    connection = await open();
    return await sendAttachmentFile({
      connection,
      sessionId: request.sessionId,
      uploadId: request.uploadId,
      file: request.file,
      onProgress: request.onProgress,
      readCoordinatorStatus,
    });
  } catch (error) {
    if (error instanceof AttachmentTransferCarrierError && !error.sentChunk && !connection?.sentChunk) return null;
    throw error;
  }
}

function validGrant(
  grant: AttachmentDirectGrant | null,
  request: AttachmentDirectUploadRequest,
  workerFp: string,
): grant is AttachmentDirectGrant {
  return grant !== null
    && grant.workerFp === workerFp
    && grant.sessionId === request.sessionId
    && grant.uploadId === request.uploadId
    && grant.filename === request.file.name
    && grant.shortPath === request.shortPath
    && grant.totalBytes === request.file.size
    && !!grant.grantId
    && !!grant.secret
    && !!grant.tabId
    && !!grant.deviceFingerprint
    && !!grant.workerEpoch;
}

async function readCoordinatorStatus(sessionId: string, uploadId: string): Promise<AttachmentTransferStatus> {
  const response = await coordClient.attachmentsDirectStatus(
    { sessionId, uploadId },
    { signal: AbortSignal.timeout(ATTACHMENT_TRANSFER_STATUS_DEADLINE_MS) },
  );
  const status = response.status;
  const bytesReceived = status ? Number(status.bytesReceived) : NaN;
  const nextSeq = status?.nextSeq ?? NaN;
  if (
    !status
    || !Number.isSafeInteger(nextSeq)
    || nextSeq < 0
    || !Number.isSafeInteger(bytesReceived)
    || bytesReceived < 0
  ) {
    throw new Error("attachment status was unavailable");
  }
  return {
    uploadId: status.uploadId,
    nextSeq,
    bytesReceived,
    lastChunkSha256: status.lastChunkSha256,
    committed: status.committed,
    absPath: status.absPath,
    error: status.error,
  };
}
