// Mints one short-lived, exact attachment grant before a direct carrier opens.
// The grant is per upload and never shares terminal direct authority or state.
// attachmentDirect keeps it only for the selected upload's loopback or peer attempt.

import { getCurrentWebKeyInfo } from "../auth/web-key.ts";
import { getTabId } from "../auth/tab-id.ts";
import { coordClient } from "../connect.ts";

export interface AttachmentDirectGrantRequest {
  readonly workerFp: string;
  readonly sessionId: string;
  readonly uploadId: string;
  readonly filename: string;
  readonly shortPath: boolean;
  readonly totalBytes: number;
}

export interface AttachmentDirectGrant extends AttachmentDirectGrantRequest {
  readonly grantId: string;
  readonly secret: string;
  readonly tabId: string;
  readonly deviceFingerprint: string;
  readonly workerEpoch: string;
  readonly peerSupported: boolean;
  readonly stunUrls: readonly string[];
}

/** A refused or unavailable grant leaves coordinator relay as the upload carrier. */
export async function mintAttachmentDirectGrant(
  request: AttachmentDirectGrantRequest,
): Promise<AttachmentDirectGrant | null> {
  try {
    const [device, tabId] = await Promise.all([getCurrentWebKeyInfo(), getTabId()]);
    const response = await coordClient.attachmentsGrantDirect({
      sessionId: request.sessionId,
      workerFp: request.workerFp,
      tabId,
      uploadId: request.uploadId,
      filename: request.filename,
      shortPath: request.shortPath,
      totalBytes: BigInt(request.totalBytes),
    });
    if (!response.grantId || !response.secret || !response.workerEpoch) return null;
    return {
      ...request,
      grantId: response.grantId,
      secret: response.secret,
      tabId,
      deviceFingerprint: device.fingerprint,
      workerEpoch: response.workerEpoch,
      peerSupported: response.peerSupported === true,
      stunUrls: [...response.stunUrls],
    };
  } catch {
    return null;
  }
}
