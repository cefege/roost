// Owns bounded typed attachment-status requests from authenticated coordinator
// handlers to exact worker handles. It correlates only metadata and durable
// receipts; attachment bytes never enter this owner or the coordinator path.
// A missing or malformed status is a worker protocol failure, never synthesized.

import { Code, ConnectError } from "@connectrpc/connect";
import { randomUUID } from "node:crypto";
import type { AttachmentTransferStatus } from "@roost/protocol/proto/attachment_transfer_pb";
import type { WAttachmentDirectStatus } from "@roost/protocol/proto/worker_transport_pb";
import {
  ATTACHMENT_TRANSFER_ERROR_REASONS,
  ATTACHMENT_TRANSFER_MAX_PENDING_STATUS_REQUESTS,
  ATTACHMENT_TRANSFER_STATUS_DEADLINE_MS,
  isAttachmentTransferChunkSha256,
} from "@roost/protocol/attachment-transfer";
import { hasAtMostUtf8Bytes } from "@roost/protocol/ui-state";
import { log } from "@roost/observability/log";
import { currentRoutableWorker } from "./worker-send-target.ts";
import type { WorkerHandle } from "./worker-registry.ts";
import { sendAttachmentDirectStatusRequest } from "./worker-send-attachment-status.ts";

const STATUS_IDENTIFIER_MAX_UTF8_BYTES = 128;

type AttachmentStatusTimer = NodeJS.Timeout;

export interface AttachmentDirectStatusResultsOptions {
  readonly clock?: AttachmentDirectStatusClock;
  readonly createRequestId?: () => string;
  readonly timeoutMs?: number;
}

export interface AttachmentDirectStatusResultSink {
  acceptStatus(source: WorkerHandle, result: WAttachmentDirectStatus): boolean;
  cancelForWorkerHandle(worker: WorkerHandle, reason: string): void;
}

export interface AttachmentDirectStatusClock {
  setTimeout(callback: () => void, delayMs: number): AttachmentStatusTimer;
  clearTimeout(timer: AttachmentStatusTimer): void;
}

interface PendingAttachmentDirectStatus {
  readonly requestId: string;
  readonly worker: WorkerHandle;
  readonly connectionGeneration: string;
  readonly sessionId: string;
  readonly uploadId: string;
  readonly promise: Promise<AttachmentTransferStatus>;
  readonly resolve: (status: AttachmentTransferStatus) => void;
  readonly reject: (error: Error) => void;
  timer: AttachmentStatusTimer | null;
}

const realAttachmentDirectStatusClock: AttachmentDirectStatusClock = {
  setTimeout: (callback, delayMs) => setTimeout(callback, delayMs),
  clearTimeout: (timer) => clearTimeout(timer),
};

/** Composition-owned typed status table; caller authorization stays at the RPC boundary. */
export class AttachmentDirectStatusResults implements AttachmentDirectStatusResultSink {
  private readonly pendingByRequestId = new Map<string, PendingAttachmentDirectStatus>();
  private readonly clock: AttachmentDirectStatusClock;
  private readonly createRequestId: () => string;
  private readonly timeoutMs: number;
  private disposed = false;

  constructor(options: AttachmentDirectStatusResultsOptions = {}) {
    this.clock = options.clock ?? realAttachmentDirectStatusClock;
    this.createRequestId = options.createRequestId ?? randomUUID;
    this.timeoutMs = options.timeoutMs ?? ATTACHMENT_TRANSFER_STATUS_DEADLINE_MS;
    if (!Number.isSafeInteger(this.timeoutMs) || this.timeoutMs <= 0) {
      throw new RangeError("attachment status timeout must be a positive safe integer");
    }
  }

  request(worker: WorkerHandle, sessionId: string, uploadId: string): Promise<AttachmentTransferStatus> {
    if (this.disposed) throw statusUnavailable("attachment status is unavailable");
    assertStatusRequestShape(sessionId, uploadId);
    if (currentRoutableWorker(worker.workerFp) !== worker) {
      throw statusUnavailable("attachment status worker is unavailable");
    }
    if (this.pendingByRequestId.size >= ATTACHMENT_TRANSFER_MAX_PENDING_STATUS_REQUESTS) {
      throw new ConnectError("attachment status capacity is exhausted", Code.ResourceExhausted);
    }
    const pending = this.reserve(worker, sessionId, uploadId);
    if (!sendAttachmentDirectStatusRequest(worker, pending)) {
      this.cancelPending(pending, statusUnavailable("attachment status worker is unavailable"));
    } else {
      log.debug("attachment-direct-status", "status_requested", {
        worker_fp: worker.workerFp,
        pending: this.pendingByRequestId.size,
      });
    }
    return pending.promise;
  }

  acceptStatus(source: WorkerHandle, result: WAttachmentDirectStatus): boolean {
    const pending = this.pendingByRequestId.get(result.requestId);
    if (!pending || !this.matchesPending(source, pending)) return false;
    const status = result.status;
    if (!status || !isValidStatus(status, pending.uploadId)) {
      this.cancelPending(pending, statusUnavailable("attachment status worker response is invalid"));
      return true;
    }
    if (!this.removePending(pending)) return false;
    pending.resolve(status);
    log.debug("attachment-direct-status", "status_accepted", {
      worker_fp: source.workerFp,
      pending: this.pendingByRequestId.size,
    });
    return true;
  }

  cancelForWorkerHandle(worker: WorkerHandle, _reason: string): void {
    for (const pending of [...this.pendingByRequestId.values()]) {
      if (pending.worker === worker) {
        this.cancelPending(pending, statusUnavailable("attachment status worker connection changed"));
      }
    }
  }

  dispose(): void {
    if (this.disposed) return;
    this.disposed = true;
    for (const pending of [...this.pendingByRequestId.values()]) {
      this.cancelPending(pending, statusUnavailable("attachment status is unavailable"));
    }
  }

  private reserve(
    worker: WorkerHandle,
    sessionId: string,
    uploadId: string,
  ): PendingAttachmentDirectStatus {
    const requestId = this.allocateRequestId();
    const deferred = Promise.withResolvers<AttachmentTransferStatus>();
    const pending: PendingAttachmentDirectStatus = {
      requestId,
      worker,
      connectionGeneration: worker.connectionGeneration,
      sessionId,
      uploadId,
      promise: deferred.promise,
      resolve: deferred.resolve,
      reject: deferred.reject,
      timer: null,
    };
    this.pendingByRequestId.set(requestId, pending);
    pending.timer = this.clock.setTimeout(() => {
      this.cancelPending(pending, new ConnectError("attachment status timed out", Code.DeadlineExceeded));
    }, this.timeoutMs);
    pending.timer.unref?.();
    return pending;
  }

  private allocateRequestId(): string {
    for (let attempt = 0; attempt < 8; attempt += 1) {
      const requestId = this.createRequestId();
      if (requestId && !this.pendingByRequestId.has(requestId)) return requestId;
    }
    throw new ConnectError("attachment status capacity is exhausted", Code.ResourceExhausted);
  }

  private matchesPending(source: WorkerHandle, pending: PendingAttachmentDirectStatus): boolean {
    return source === pending.worker
      && source.connectionGeneration === pending.connectionGeneration
      && currentRoutableWorker(source.workerFp) === source;
  }

  private cancelPending(pending: PendingAttachmentDirectStatus, error: Error): void {
    if (!this.removePending(pending)) return;
    pending.reject(error);
    log.debug("attachment-direct-status", "status_cancelled", {
      worker_fp: pending.worker.workerFp,
      pending: this.pendingByRequestId.size,
    });
  }

  private removePending(pending: PendingAttachmentDirectStatus): boolean {
    if (this.pendingByRequestId.get(pending.requestId) !== pending) return false;
    this.pendingByRequestId.delete(pending.requestId);
    if (pending.timer !== null) this.clock.clearTimeout(pending.timer);
    pending.timer = null;
    return true;
  }
}

function assertStatusRequestShape(sessionId: string, uploadId: string): void {
  if (
    sessionId.length === 0
    || !hasAtMostUtf8Bytes(sessionId, STATUS_IDENTIFIER_MAX_UTF8_BYTES)
    || uploadId.length === 0
    || !hasAtMostUtf8Bytes(uploadId, STATUS_IDENTIFIER_MAX_UTF8_BYTES)
    || /[\\/\x00-\x1f\x7f]/u.test(uploadId)
  ) throw new ConnectError("attachment status request is invalid", Code.InvalidArgument);
}

function isValidStatus(status: AttachmentTransferStatus, uploadId: string): boolean {
  return status.uploadId === uploadId
    && Number.isSafeInteger(status.nextSeq)
    && status.nextSeq >= 0
    && status.bytesReceived >= 0n
    && status.bytesReceived <= BigInt(Number.MAX_SAFE_INTEGER)
    && (status.lastChunkSha256 === "" || isAttachmentTransferChunkSha256(status.lastChunkSha256))
    && (status.error === "" || (ATTACHMENT_TRANSFER_ERROR_REASONS as readonly string[]).includes(status.error))
    && (!status.committed || status.error === "")
    && (status.committed || status.absPath === "");
}

function statusUnavailable(message: string): ConnectError {
  return new ConnectError(message, Code.Unavailable);
}
