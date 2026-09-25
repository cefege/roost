// Public attachment-upload facade for coordinator relay and direct carriers.
// AttachmentOperationOwner is the sole byte destination and durable receipt owner;
// this module preserves the coordinator reply shape while exposing direct status.

import { log } from "@roost/observability/log";
import { AttachmentOperationOwner, type AttachmentOperationResult } from "./attachment-operation-owner.ts";
import type {
  AttachmentOperationError,
  AttachmentOperationReceipt,
  AttachmentOperationStatus,
} from "./attachment-operation-receipts.ts";
import { syncAttachmentOperationProgress } from "./attachment-operation-journal.ts";
export { probeAttachment, recordAttachmentHash } from "./attachment-file-store.ts";

export interface AttachmentChunk {
  readonly request_id: string;
  readonly session_id: string;
  readonly filename: string;
  readonly short_path: boolean;
  readonly data: Uint8Array;
  readonly last: boolean;
  readonly seq: number;
  /** Direct hello binds this; coordinator relay remains unset. */
  readonly total_bytes?: number;
  /** Direct attachment frames carry an exact byte position. */
  readonly offset?: number;
  /** Direct attachment frames carry the SHA-256 of this exact chunk. */
  readonly chunk_sha256?: string;
  readonly carrier?: "coordinator" | "direct";
  readonly carrier_id?: string;
}

export interface AttachmentReply {
  ok: (absPath: string, receipt: AttachmentOperationReceipt) => void;
  err: (message: string, error?: AttachmentOperationError) => void;
  /** Direct carriers ACK every non-final synchronous destination write. */
  progress?: (receipt: AttachmentOperationReceipt) => void;
}

const attachmentOperations = new AttachmentOperationOwner();

// unref() so an idle sweep never holds a test process or idle worker open.
setInterval(() => attachmentOperations.sweepIdle(), 60_000).unref();

const ERROR_MESSAGE: Record<AttachmentOperationError, string> = {
  upload_not_found: "upload is unavailable",
  upload_mismatch: "upload metadata does not match",
  chunk_out_of_order: "attachment chunk is out of order",
  chunk_offset_mismatch: "attachment chunk offset does not match",
  chunk_sha256_mismatch: "attachment chunk digest does not match",
  total_bytes_mismatch: "attachment size does not match declared total",
  write_failed: "attachment write failed",
};

/** Relay chunks write before any await, so WebSocket arrival order stays write order. */
export function handleAttachmentChunk(chunk: AttachmentChunk, reply: AttachmentReply): void {
  void acceptAttachmentChunk(chunk)
    .then((result) => { deliverAttachmentResult(reply, result); })
    .catch((error: unknown) => {
      log.warn("worker", "attachment_reply_failed", { request_id: chunk.request_id, error: String(error) });
    });
}

/** Direct ACKs wait on async fsync so durability never blocks unrelated worker RPCs. */
export async function handleDirectAttachmentChunk(
  chunk: AttachmentChunk,
  reply: AttachmentReply,
): Promise<void> {
  const result = await acceptAttachmentChunk(chunk);
  if (result.kind === "receipt" && !result.receipt.committed) {
    try {
      await syncAttachmentOperationProgress(chunk.session_id, chunk.request_id);
    } catch {
      reply.err(ERROR_MESSAGE.write_failed, "write_failed");
      return;
    }
  }
  deliverAttachmentResult(reply, result);
}

export function attachmentOperationStatus(sessionId: string, uploadId: string): AttachmentOperationStatus {
  return attachmentOperations.status(sessionId, uploadId);
}

/** A dead direct carrier releases only its fd; its durable status remains queryable. */
export function detachDirectAttachmentCarrier(carrierId: string): void {
  attachmentOperations.detachDirectCarrier(carrierId);
}

function acceptAttachmentChunk(chunk: AttachmentChunk): Promise<AttachmentOperationResult> {
  // Relay chunks carry no offset; their position is the operation's written length.
  const offset = chunk.offset
    ?? (chunk.seq === 0 ? 0 : attachmentOperations.status(chunk.session_id, chunk.request_id).bytesReceived);
  return attachmentOperations.accept({
    requestId: chunk.request_id,
    sessionId: chunk.session_id,
    filename: chunk.filename,
    shortPath: chunk.short_path,
    totalBytes: chunk.total_bytes,
    carrier: chunk.carrier ?? "coordinator",
    carrierId: chunk.carrier_id ?? "",
    seq: chunk.seq,
    offset,
    data: chunk.data,
    last: chunk.last,
    chunkSha256: chunk.chunk_sha256 ?? sha256(chunk.data),
  });
}

function deliverAttachmentResult(
  reply: AttachmentReply,
  result: AttachmentOperationResult,
): void {
  if (result.kind === "error") {
    reply.err(ERROR_MESSAGE[result.error], result.error);
    return;
  }
  if (result.receipt.committed) {
    reply.ok(result.receipt.absPath, result.receipt);
    return;
  }
  reply.progress?.(result.receipt);
}

function sha256(data: Uint8Array): string {
  return new Bun.CryptoHasher("sha256").update(data).digest("hex");
}
