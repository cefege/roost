// Direct attachment upload mechanics shared by loopback and WebRTC carriers.
// Route selection opens one carrier before this module serially hashes and sends slices.
// It owns exact ACK/status validation and closes the chosen carrier after settlement.

import {
  ATTACHMENT_TRANSFER_COMPLETE_REASON,
  ATTACHMENT_TRANSFER_DIRECT_CHUNK_BYTES,
  isAttachmentTransferChunkSha256,
} from "@roost/protocol/attachment-transfer";

export interface AttachmentTransferChunk {
  readonly uploadId: string;
  readonly seq: number;
  readonly offset: bigint;
  readonly data: Uint8Array;
  readonly chunkSha256: string;
  readonly last: boolean;
}

export interface AttachmentTransferAck {
  readonly bytesReceived: number;
  readonly absPath: string;
  readonly chunkSha256: string;
}

export interface AttachmentTransferStatus {
  readonly uploadId: string;
  readonly nextSeq: number;
  readonly bytesReceived: number;
  readonly lastChunkSha256: string;
  readonly committed: boolean;
  readonly absPath: string;
  readonly error: string;
}

export interface AttachmentTransferConnection {
  readonly sentChunk: boolean;
  sendChunk(chunk: AttachmentTransferChunk): Promise<AttachmentTransferAck>;
  requestStatus(uploadId: string): Promise<AttachmentTransferStatus>;
  close(reason: string): void;
}

export interface AttachmentFileTransferOptions {
  readonly connection: AttachmentTransferConnection;
  readonly sessionId: string;
  readonly uploadId: string;
  readonly file: File;
  readonly onProgress?: (bytesSent: number) => void;
  readonly readCoordinatorStatus: (sessionId: string, uploadId: string) => Promise<AttachmentTransferStatus>;
}

export interface AttachmentTransferResult {
  readonly abs_path: string;
}

/** A carrier failure records whether upload bytes left the browser and ACK state is ambiguous. */
export class AttachmentTransferCarrierError extends Error {
  constructor(
    message: string,
    readonly sentChunk: boolean,
    readonly ambiguous = false,
  ) {
    super(message);
    this.name = "AttachmentTransferCarrierError";
  }
}

/** Sends one ordered direct upload, including the required empty final chunk. */
export async function sendAttachmentFile(
  options: AttachmentFileTransferOptions,
): Promise<AttachmentTransferResult> {
  const { connection, file, onProgress, uploadId } = options;
  let completed = false;
  try {
    let absPath = "";
    let seq = 0;
    for (let offset = 0; offset === 0 || offset < file.size; offset += ATTACHMENT_TRANSFER_DIRECT_CHUNK_BYTES) {
      const data = new Uint8Array(await file.slice(offset, offset + ATTACHMENT_TRANSFER_DIRECT_CHUNK_BYTES).arrayBuffer());
      const last = offset + ATTACHMENT_TRANSFER_DIRECT_CHUNK_BYTES >= file.size;
      const chunk = {
        uploadId,
        seq,
        offset: BigInt(offset),
        data,
        chunkSha256: await hashChunk(data, connection.sentChunk),
        last,
      } satisfies AttachmentTransferChunk;
      const expectedBytes = offset + data.byteLength;
      let ack: AttachmentTransferAck;
      try {
        ack = await connection.sendChunk(chunk);
      } catch (error) {
        if (!(error instanceof AttachmentTransferCarrierError) || !error.ambiguous) throw error;
        ack = await recoverAcknowledgement(options, chunk, expectedBytes);
      }
      validateAcknowledgement(ack, chunk, expectedBytes, last);
      onProgress?.(ack.bytesReceived);
      if (last) absPath = ack.absPath;
      seq += 1;
    }
    completed = true;
    return { abs_path: absPath };
  } finally {
    try {
      connection.close(completed ? ATTACHMENT_TRANSFER_COMPLETE_REASON : "attachment transfer failed");
    } catch {
      // The upload outcome is already settled; close is only resource release.
    }
  }
}

async function recoverAcknowledgement(
  options: AttachmentFileTransferOptions,
  chunk: AttachmentTransferChunk,
  expectedBytes: number,
): Promise<AttachmentTransferAck> {
  try {
    const status = await options.connection.requestStatus(chunk.uploadId);
    const acknowledgement = acknowledgementFromStatus(status, chunk, expectedBytes);
    if (acknowledgement) return acknowledgement;
  } catch {
    // A lost carrier cannot answer status; the authenticated coordinator relays it.
  }
  try {
    const status = await options.readCoordinatorStatus(options.sessionId, chunk.uploadId);
    const acknowledgement = acknowledgementFromStatus(status, chunk, expectedBytes);
    if (acknowledgement) return acknowledgement;
  } catch {
    // The original direct send remains unconfirmed when status control is unavailable.
  }
  throw new AttachmentTransferCarrierError("attachment transfer acknowledgement was not confirmed", true);
}

function acknowledgementFromStatus(
  status: AttachmentTransferStatus,
  chunk: AttachmentTransferChunk,
  expectedBytes: number,
): AttachmentTransferAck | null {
  if (
    status.error
    || status.uploadId !== chunk.uploadId
    || status.nextSeq !== chunk.seq + 1
    || status.bytesReceived !== expectedBytes
    || !isAttachmentTransferChunkSha256(status.lastChunkSha256)
    || status.lastChunkSha256 !== chunk.chunkSha256
    || status.committed !== chunk.last
    || (!!status.absPath) !== chunk.last
  ) return null;
  return {
    bytesReceived: status.bytesReceived,
    absPath: status.absPath,
    chunkSha256: status.lastChunkSha256,
  };
}

function validateAcknowledgement(
  ack: AttachmentTransferAck,
  chunk: AttachmentTransferChunk,
  expectedBytes: number,
  last: boolean,
): void {
  if (
    ack.bytesReceived !== expectedBytes
    || !isAttachmentTransferChunkSha256(ack.chunkSha256)
    || ack.chunkSha256 !== chunk.chunkSha256
    || (!!ack.absPath) !== last
  ) throw new AttachmentTransferCarrierError("attachment transfer acknowledged invalid chunk state", true);
}

async function hashChunk(data: Uint8Array, sentChunk: boolean): Promise<string> {
  try {
    const ownedBytes = new Uint8Array(data.byteLength);
    ownedBytes.set(data);
    const digest = await crypto.subtle.digest("SHA-256", ownedBytes.buffer);
    return Array.from(new Uint8Array(digest), (byte) => byte.toString(16).padStart(2, "0")).join("");
  } catch {
    throw new AttachmentTransferCarrierError("attachment transfer could not hash a chunk", sentChunk);
  }
}
