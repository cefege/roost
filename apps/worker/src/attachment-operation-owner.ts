// Single streamed-attachment operation owner for coordinator and direct carriers.
// Direct chunks sync bytes and journal before their ACK; coordinator chunks retain
// in-memory throughput until their durable final commit preserves the exact outcome.
// Every fsync on the accept path is asynchronous, so a slow disk never stalls the
// worker event loop; an idle sweep releases operations whose carrier went silent.

import fs from "node:fs";
import { log } from "@roost/shared/log";
import { isAttachmentTransferChunkSha256 } from "@roost/shared/attachment-transfer";
import {
  attachmentReplyPath,
  commitAttachmentDestination,
  placeAttachmentDestination,
  recordAttachmentHash,
  reserveAttachmentDestination,
  syncAttachmentDirectoryAsync,
  syncAttachmentFileAsync,
} from "./attachment-file-store.ts";
import {
  hashAttachmentFile,
  sha256AttachmentFile,
} from "./attachment-file-hash.ts";
import {
  createAttachmentOperation,
  loadAttachmentOperation,
  persistAttachmentOperation,
  removeAttachmentTemp,
  type AttachmentOperationCarrier,
  type AttachmentOperationDescriptor,
  type AttachmentOperationJournal,
  type AttachmentOperationPaths,
} from "./attachment-operation-journal.ts";
import {
  emptyStatus,
  journalError,
  receiptFromJournal,
  statusFromJournal,
  type AttachmentOperationError,
  type AttachmentOperationReceipt,
  type AttachmentOperationStatus,
} from "./attachment-operation-receipts.ts";

export interface AttachmentOperationChunk extends AttachmentOperationDescriptor {
  readonly carrier: AttachmentOperationCarrier;
  readonly carrierId: string;
  readonly seq: number;
  readonly offset: number;
  readonly data: Uint8Array;
  readonly last: boolean;
  readonly chunkSha256: string;
}

type AttachmentOperationFailure = {
  readonly kind: "error";
  readonly error: AttachmentOperationError;
};

export type AttachmentOperationResult =
  | { readonly kind: "receipt"; readonly receipt: AttachmentOperationReceipt }
  | AttachmentOperationFailure;

interface ActiveOperation {
  readonly key: string;
  readonly paths: AttachmentOperationPaths;
  readonly journal: AttachmentOperationJournal;
  fd: number;
  readonly hasher: Bun.CryptoHasher;
  lastActivityMs: number;
  /** Set while the final chunk's durable commit awaits disk; the operation stays registered. */
  commit: Promise<AttachmentOperationResult> | null;
}

/** Matches the coordinator's pending-RPC deadline: once it gives up, so does the worker. */
export const ATTACHMENT_OPERATION_IDLE_MS = 5 * 60 * 1000;

type AttachmentOperationPreparation =
  | { readonly kind: "operation"; readonly operation: ActiveOperation }
  | AttachmentOperationFailure;

/** Process owner for all attachment byte carriers. */
export class AttachmentOperationOwner {
  private readonly active = new Map<string, ActiveOperation>();

  constructor(private readonly nowMs: () => number = Date.now) {}

  /** Non-final chunks settle before the first await, so arrival order is write order. */
  async accept(chunk: AttachmentOperationChunk): Promise<AttachmentOperationResult> {
    let operation: ActiveOperation | undefined;
    try {
      if (!validChunk(chunk)) return { kind: "error", error: "upload_mismatch" };
      if (!isAttachmentTransferChunkSha256(chunk.chunkSha256)) {
        return { kind: "error", error: "chunk_sha256_mismatch" };
      }
      const prepared = this.prepare(chunk);
      if (prepared.kind === "error") return prepared;
      operation = prepared.operation;
      if (operation.commit) {
        await operation.commit;
        return await this.accept(chunk);
      }
      operation.lastActivityMs = this.nowMs();
      const journal = operation.journal;
      if (journal.error) return { kind: "error", error: journalError(journal) };
      if (journal.finalName && !journal.committed && !this.recoverFinalization(operation.paths, journal)) {
        return { kind: "error", error: journalError(journal) };
      }
      const actualChunkSha256 = sha256(chunk.data);
      if (actualChunkSha256 !== chunk.chunkSha256) return this.fail(operation, "chunk_sha256_mismatch");
      if (isLastAcceptedDuplicate(journal, chunk)) return { kind: "receipt", receipt: receiptFromJournal(journal) };
      if (journal.committed || chunk.seq !== journal.nextSeq) return this.fail(operation, "chunk_out_of_order");
      if (chunk.offset !== journal.bytesWritten) return this.fail(operation, "chunk_offset_mismatch");
      if (
        journal.totalBytes !== undefined
        && chunk.data.byteLength > journal.totalBytes - journal.bytesWritten
      ) return this.fail(operation, "total_bytes_mismatch");
      const recordsDirectStatus = chunk.carrier === "direct";
      writeAllSync(operation.fd, chunk.data);
      operation.hasher.update(chunk.data);
      journal.nextSeq += 1;
      journal.bytesWritten += chunk.data.byteLength;
      journal.lastChunkSha256 = chunk.chunkSha256;
      journal.lastChunkFinal = chunk.last;
      if (!chunk.last) {
        if (recordsDirectStatus) persistAttachmentOperation(operation.paths, journal, false);
        return { kind: "receipt", receipt: receiptFromJournal(journal) };
      }
      if (journal.totalBytes !== undefined && journal.bytesWritten !== journal.totalBytes) {
        return this.fail(operation, "total_bytes_mismatch");
      }
      operation.commit = this.commitFinal(operation);
      return await operation.commit;
    } catch {
      if (operation?.journal.finalName && !operation.journal.committed && !operation.journal.error) {
        this.closeActive(operation);
        return { kind: "error", error: "write_failed" };
      }
      return operation ? this.fail(operation, "write_failed") : { kind: "error", error: "write_failed" };
    }
  }

  /** Reads journal counters only; a partial upload is never reopened or rehashed here. */
  status(sessionId: string, uploadId: string): AttachmentOperationStatus {
    const active = this.active.get(operationKey(sessionId, uploadId));
    if (active) return statusFromJournal(active.journal);
    const loaded = loadAttachmentOperation(sessionId, uploadId);
    if (loaded.kind === "missing") return emptyStatus(uploadId, "upload_not_found");
    if (loaded.kind === "invalid") return emptyStatus(uploadId, "write_failed");
    const { paths, journal } = loaded;
    if (!journal.error && journal.finalName && !journal.committed) {
      this.recoverFinalization(paths, journal);
    } else if (!journal.error && !journal.finalName && !tempMatchesJournal(paths, journal)) {
      this.failJournal(paths, journal, "write_failed");
    }
    return statusFromJournal(journal);
  }

  detachDirectCarrier(carrierId: string): void {
    for (const operation of [...this.active.values()]) {
      if (operation.journal.carrier !== "direct" || operation.journal.carrierId !== carrierId) continue;
      if (operation.commit === null) this.closeActive(operation);
    }
  }

  /**
   * Coordinator relay progress is not journaled, so an idle relay operation can
   * never resume and is failed; an idle direct operation keeps its durable status.
   */
  sweepIdle(): void {
    const now = this.nowMs();
    for (const operation of [...this.active.values()]) {
      if (operation.commit || now - operation.lastActivityMs <= ATTACHMENT_OPERATION_IDLE_MS) continue;
      log.warn("worker", "attachment_stream_abandoned", {
        request_id: operation.journal.requestId,
        carrier: operation.journal.carrier,
        bytes: operation.journal.bytesWritten,
      });
      if (operation.journal.carrier === "coordinator") this.fail(operation, "upload_not_found");
      else this.closeActive(operation);
    }
  }

  private async commitFinal(operation: ActiveOperation): Promise<AttachmentOperationResult> {
    const { journal, paths } = operation;
    try {
      this.closeFd(operation);
      // Reservation, journal write, and rename run without yielding, so no other
      // upload can reserve the same destination name before this one occupies it.
      const destination = reserveAttachmentDestination(paths.sessionDir, journal.filename);
      journal.finalName = destination.fileName;
      journal.contentSha256 = operation.hasher.digest("hex");
      persistAttachmentOperation(paths, journal, false);
      const placed = placeAttachmentDestination(paths.sessionDir, paths.tempPath, journal.finalName, journal.contentSha256);
      // Round one makes the bytes and their name durable; only then may round two
      // make a committed journal durable, so status never reports unflushed bytes.
      await Promise.all([syncAttachmentFileAsync(placed.filePath), syncAttachmentDirectoryAsync(paths.sessionDir)]);
      recordAttachmentHash(paths.sessionDir, journal.contentSha256, journal.finalName);
      journal.absPath = attachmentReplyPath(paths.sessionDir, placed.filePath, journal.shortPath);
      journal.committed = true;
      persistAttachmentOperation(paths, journal, false);
      await Promise.all([syncAttachmentFileAsync(paths.journalPath), syncAttachmentDirectoryAsync(paths.operationDir)]);
      log.info("worker", "attachment_stream_saved", { session_id: journal.sessionId, size: journal.bytesWritten });
      return { kind: "receipt", receipt: receiptFromJournal(journal) };
    } catch {
      // A reserved name leaves a recoverable journal; status finishes the commit later.
      if (journal.finalName && !journal.committed && !journal.error) return { kind: "error", error: "write_failed" };
      journal.committed = false; // a commit whose flush failed is never reported as a receipt
      return this.failJournal(paths, journal, "write_failed");
    } finally {
      this.closeActive(operation);
    }
  }

  private prepare(chunk: AttachmentOperationChunk): AttachmentOperationPreparation {
    const key = operationKey(chunk.sessionId, chunk.requestId);
    const active = this.active.get(key);
    if (active) {
      if (!sameOperationDescriptor(active.journal, chunk) || !sameCarrier(active.journal, chunk)) {
        return { kind: "error", error: "upload_mismatch" };
      }
      return { kind: "operation", operation: active };
    }
    const loaded = loadAttachmentOperation(chunk.sessionId, chunk.requestId);
    if (loaded.kind === "invalid") return { kind: "error", error: "write_failed" };
    if (loaded.kind === "missing") {
      if (chunk.seq !== 0) return { kind: "error", error: "chunk_out_of_order" };
      if (chunk.offset !== 0) return { kind: "error", error: "chunk_offset_mismatch" };
      const created = createAttachmentOperation(chunk, chunk.carrier, chunk.carrierId);
      if (!created) return { kind: "error", error: "upload_not_found" };
      try {
        const operation: ActiveOperation = {
          key,
          paths: created.paths,
          journal: created.journal,
          fd: fs.openSync(created.paths.tempPath, "w", 0o600),
          hasher: new Bun.CryptoHasher("sha256"),
          lastActivityMs: this.nowMs(),
          commit: null,
        };
        this.active.set(key, operation);
        return { kind: "operation", operation };
      } catch {
        return this.failJournal(created.paths, created.journal, "write_failed");
      }
    }
    if (!sameOperationDescriptor(loaded.journal, chunk) || !sameCarrier(loaded.journal, chunk)) {
      return { kind: "error", error: "upload_mismatch" };
    }
    const operation = this.openLoaded(loaded.paths, loaded.journal);
    if (!operation) return { kind: "error", error: journalError(loaded.journal) };
    return { kind: "operation", operation };
  }

  private openLoaded(paths: AttachmentOperationPaths, journal: AttachmentOperationJournal): ActiveOperation | null {
    const key = operationKey(journal.sessionId, journal.requestId);
    const active = this.active.get(key);
    if (active) return active;
    if (journal.error || journal.committed || journal.finalName) {
      return { key, paths, journal, fd: -1, hasher: new Bun.CryptoHasher("sha256"), lastActivityMs: this.nowMs(), commit: null };
    }
    try {
      if (!tempMatchesJournal(paths, journal)) throw new Error("attachment temp mismatch");
      const operation: ActiveOperation = {
        key,
        paths,
        journal,
        fd: fs.openSync(paths.tempPath, "r+", 0o600),
        hasher: hashAttachmentFile(paths.tempPath),
        lastActivityMs: this.nowMs(),
        commit: null,
      };
      this.active.set(key, operation);
      return operation;
    } catch {
      this.failJournal(paths, journal, "write_failed");
      return null;
    }
  }

  private recoverFinalization(paths: AttachmentOperationPaths, journal: AttachmentOperationJournal): boolean {
    try {
      const committedDestination = commitAttachmentDestination(
        paths.sessionDir,
        paths.tempPath,
        journal.finalName,
        journal.contentSha256,
      );
      if (
        !committedDestination.verifiedExistingFile
        && sha256AttachmentFile(committedDestination.filePath) !== journal.contentSha256
      ) {
        this.failJournal(paths, journal, "write_failed");
        return false;
      }
      journal.absPath = attachmentReplyPath(paths.sessionDir, committedDestination.filePath, journal.shortPath);
      journal.committed = true;
      persistAttachmentOperation(paths, journal);
      return true;
    } catch {
      this.failJournal(paths, journal, "write_failed");
      return false;
    }
  }

  private fail(operation: ActiveOperation, error: AttachmentOperationError): AttachmentOperationResult {
    return this.failJournal(operation.paths, operation.journal, error, operation);
  }

  private failJournal(
    paths: AttachmentOperationPaths,
    journal: AttachmentOperationJournal,
    error: AttachmentOperationError,
    operation?: ActiveOperation,
  ): AttachmentOperationFailure {
    journal.error = error;
    try { persistAttachmentOperation(paths, journal); } catch { /* failure remains non-committed */ }
    if (operation) this.closeActive(operation);
    removeAttachmentTemp(paths);
    return { kind: "error", error };
  }

  private closeActive(operation: ActiveOperation): void {
    this.closeFd(operation);
    this.active.delete(operation.key);
  }

  private closeFd(operation: ActiveOperation): void {
    if (operation.fd < 0) return;
    try { fs.closeSync(operation.fd); } catch { /* descriptor was already closed */ }
    operation.fd = -1;
  }
}

function validChunk(chunk: AttachmentOperationChunk): boolean {
  return Number.isSafeInteger(chunk.seq)
    && chunk.seq >= 0
    && Number.isSafeInteger(chunk.offset)
    && chunk.offset >= 0
    && validTotal(chunk.totalBytes)
    && chunk.carrierId.length <= 128;
}

function validTotal(totalBytes: number | undefined): boolean {
  return totalBytes === undefined || Number.isSafeInteger(totalBytes) && totalBytes >= 0;
}

function sameOperationDescriptor(journal: AttachmentOperationJournal, chunk: AttachmentOperationChunk): boolean {
  return journal.sessionId === chunk.sessionId
    && journal.filename === chunk.filename
    && journal.shortPath === chunk.shortPath
    && journal.totalBytes === chunk.totalBytes;
}

function sameCarrier(journal: AttachmentOperationJournal, chunk: AttachmentOperationChunk): boolean {
  return journal.carrier === chunk.carrier && journal.carrierId === chunk.carrierId;
}

function tempMatchesJournal(paths: AttachmentOperationPaths, journal: AttachmentOperationJournal): boolean {
  try {
    const stat = fs.statSync(paths.tempPath);
    return stat.isFile() && stat.size === journal.bytesWritten;
  } catch {
    return false;
  }
}

function isLastAcceptedDuplicate(journal: AttachmentOperationJournal, chunk: AttachmentOperationChunk): boolean {
  return journal.nextSeq > 0
    && chunk.seq === journal.nextSeq - 1
    && chunk.offset + chunk.data.byteLength === journal.bytesWritten
    && chunk.chunkSha256 === journal.lastChunkSha256
    && chunk.last === journal.lastChunkFinal;
}

function writeAllSync(fd: number, data: Uint8Array): void {
  let offset = 0;
  while (offset < data.byteLength) {
    const written = fs.writeSync(fd, data, offset, data.byteLength - offset);
    if (written <= 0) throw new Error("attachment write made no progress");
    offset += written;
  }
}

function sha256(data: Uint8Array): string {
  return new Bun.CryptoHasher("sha256").update(data).digest("hex");
}

function operationKey(sessionId: string, requestId: string): string {
  return `${sessionId}\u0000${requestId}`;
}
