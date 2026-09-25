// Receipt and status projections of one durable attachment operation journal.
// AttachmentOperationOwner answers carriers with these shapes; the upload facade
// and direct sockets consume them. Pure functions: no fd, disk, or clock access.

import type { AttachmentTransferErrorReason } from "@roost/protocol/attachment-transfer";
import type { AttachmentOperationJournal } from "./attachment-operation-journal.ts";

export type AttachmentOperationError = Extract<
  AttachmentTransferErrorReason,
  | "upload_not_found"
  | "upload_mismatch"
  | "chunk_out_of_order"
  | "chunk_offset_mismatch"
  | "chunk_sha256_mismatch"
  | "total_bytes_mismatch"
  | "write_failed"
>;

export interface AttachmentOperationReceipt {
  readonly seq: number;
  readonly nextSeq: number;
  readonly bytesReceived: number;
  readonly chunkSha256: string;
  readonly committed: boolean;
  readonly absPath: string;
}

export interface AttachmentOperationStatus {
  readonly uploadId: string;
  readonly nextSeq: number;
  readonly bytesReceived: number;
  readonly lastChunkSha256: string;
  readonly committed: boolean;
  readonly absPath: string;
  readonly error: AttachmentOperationError | "";
}

export function receiptFromJournal(journal: AttachmentOperationJournal): AttachmentOperationReceipt {
  return {
    seq: journal.nextSeq - 1,
    nextSeq: journal.nextSeq,
    bytesReceived: journal.bytesWritten,
    chunkSha256: journal.lastChunkSha256,
    committed: journal.committed,
    absPath: journal.absPath,
  };
}

export function statusFromJournal(journal: AttachmentOperationJournal): AttachmentOperationStatus {
  return {
    uploadId: journal.requestId,
    nextSeq: journal.nextSeq,
    bytesReceived: journal.bytesWritten,
    lastChunkSha256: journal.lastChunkSha256,
    committed: journal.committed,
    absPath: journal.absPath,
    error: journal.error ? journalError(journal) : "",
  };
}

export function emptyStatus(uploadId: string, error: AttachmentOperationError): AttachmentOperationStatus {
  return {
    uploadId,
    nextSeq: 0,
    bytesReceived: 0,
    lastChunkSha256: "",
    committed: false,
    absPath: "",
    error,
  };
}

/** A journal written by an older or foreign build maps unknown errors to write_failed. */
export function journalError(journal: AttachmentOperationJournal): AttachmentOperationError {
  return isOperationError(journal.error) ? journal.error : "write_failed";
}

function isOperationError(value: string): value is AttachmentOperationError {
  return value === "upload_not_found"
    || value === "upload_mismatch"
    || value === "chunk_out_of_order"
    || value === "chunk_offset_mismatch"
    || value === "chunk_sha256_mismatch"
    || value === "total_bytes_mismatch"
    || value === "write_failed";
}
