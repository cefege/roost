// One authenticated loopback transport for one direct attachment upload.
// attachmentDirect selects it only for a fingerprint-matching local worker door.
// It authenticates an exact attachment grant and settles chunk ACKs or status receipts.

import { create, fromBinary, toBinary } from "@bufbuild/protobuf";
import {
  ATTACHMENT_TRANSFER_ACK_DEADLINE_MS,
  ATTACHMENT_TRANSFER_HELLO_DEADLINE_MS,
  ATTACHMENT_TRANSFER_LOOPBACK_PATH,
  ATTACHMENT_TRANSFER_LOOPBACK_SUBPROTOCOL,
  ATTACHMENT_TRANSFER_STATUS_DEADLINE_MS,
} from "@roost/protocol/attachment-transfer";
import {
  AttachmentTransferChunkSchema,
  AttachmentTransferClientFrameSchema,
  AttachmentTransferHelloSchema,
  AttachmentTransferServerFrameSchema,
  AttachmentTransferStatusRequestSchema,
  type AttachmentTransferClientFrame,
  type AttachmentTransferReady,
  type AttachmentTransferStatus as ProtocolAttachmentTransferStatus,
} from "@roost/protocol/proto/attachment_transfer_pb";
import type { LocalWorkerDoor } from "./localWorkerDiscovery.ts";
import type { AttachmentDirectGrant } from "../attachments/attachmentDirectGrant.ts";
import {
  AttachmentTransferCarrierError,
  type AttachmentTransferAck,
  type AttachmentTransferChunk,
  type AttachmentTransferConnection,
  type AttachmentTransferStatus,
} from "../attachments/attachmentTransfer.ts";

export interface AttachmentLoopbackTransferOptions {
  readonly door: LocalWorkerDoor;
  readonly grant: AttachmentDirectGrant;
}

export interface AttachmentLoopbackTransferDependencies {
  readonly createSocket?: (url: string, protocol: string) => WebSocket;
}

interface Deferred<T> {
  readonly promise: Promise<T>;
  readonly resolve: (value: T) => void;
  readonly reject: (error: Error) => void;
}

interface TimedWaiter<T> extends Deferred<T> {
  timer: ReturnType<typeof setTimeout> | null;
}

interface AckWaiter extends TimedWaiter<AttachmentTransferAck> {
  readonly seq: number;
}

interface StatusWaiter extends TimedWaiter<AttachmentTransferStatus> {
  readonly uploadId: string;
}

/** Opens a distinct loopback socket; callers close it once the upload settles. */
export function openAttachmentLoopbackTransfer(
  options: AttachmentLoopbackTransferOptions,
  dependencies: AttachmentLoopbackTransferDependencies = {},
): Promise<AttachmentTransferConnection> {
  return new AttachmentLoopbackTransfer(options, dependencies).open();
}

class AttachmentLoopbackTransfer implements AttachmentTransferConnection {
  private readonly readyDeferred = deferred<void>();
  private socket: WebSocket | null = null;
  private setupTimer: ReturnType<typeof setTimeout> | null = null;
  private ackWaiter: AckWaiter | null = null;
  private statusWaiter: StatusWaiter | null = null;
  private ready = false;
  private closed = false;
  sentChunk = false;

  constructor(
    private readonly options: AttachmentLoopbackTransferOptions,
    private readonly dependencies: AttachmentLoopbackTransferDependencies,
  ) {}

  open(): Promise<AttachmentTransferConnection> {
    this.setupTimer = setTimeout(
      () => this.finish("attachment loopback setup timed out"),
      ATTACHMENT_TRANSFER_HELLO_DEADLINE_MS,
    );
    try {
      const socket = this.createSocket(this.socketUrl(), ATTACHMENT_TRANSFER_LOOPBACK_SUBPROTOCOL);
      socket.binaryType = "arraybuffer";
      socket.onopen = () => this.sendHello();
      socket.onmessage = (event) => this.receive(event.data);
      socket.onclose = () => this.finish("attachment loopback closed");
      socket.onerror = () => this.finish("attachment loopback failed");
      this.socket = socket;
    } catch {
      this.finish("attachment loopback could not open");
    }
    return this.readyDeferred.promise.then(() => this);
  }

  sendChunk(chunk: AttachmentTransferChunk): Promise<AttachmentTransferAck> {
    const socket = this.socket;
    if (this.closed || !this.ready || !socket || socket.readyState !== 1 || this.ackWaiter !== null) {
      return Promise.reject(new AttachmentTransferCarrierError("attachment loopback cannot send a chunk", this.sentChunk));
    }
    const waiter: AckWaiter = { ...deferred<AttachmentTransferAck>(), seq: chunk.seq, timer: null };
    this.ackWaiter = waiter;
    const frame = toBinary(AttachmentTransferClientFrameSchema, create(AttachmentTransferClientFrameSchema, {
      frame: { case: "chunk", value: create(AttachmentTransferChunkSchema, chunk) },
    }));
    const sentBefore = this.sentChunk;
    this.sentChunk = true;
    try {
      socket.send(frame);
      if (this.ackWaiter === waiter) {
        waiter.timer = setTimeout(
          () => this.rejectAck(waiter, "attachment loopback acknowledgement timed out", true),
          ATTACHMENT_TRANSFER_ACK_DEADLINE_MS,
        );
      }
    } catch {
      this.sentChunk = sentBefore;
      if (this.ackWaiter === waiter) this.rejectAck(waiter, "attachment loopback could not send a chunk");
    }
    return waiter.promise;
  }

  requestStatus(uploadId: string): Promise<AttachmentTransferStatus> {
    if (this.closed || !this.ready || this.statusWaiter !== null) {
      return Promise.reject(new AttachmentTransferCarrierError("attachment loopback cannot request status", this.sentChunk));
    }
    const waiter: StatusWaiter = { ...deferred<AttachmentTransferStatus>(), uploadId, timer: null };
    this.statusWaiter = waiter;
    if (!this.sendFrame({
      case: "statusRequest",
      value: create(AttachmentTransferStatusRequestSchema, { uploadId }),
    })) {
      this.rejectStatus(waiter, "attachment loopback could not request status");
      return waiter.promise;
    }
    if (this.statusWaiter === waiter) {
      waiter.timer = setTimeout(
        () => this.rejectStatus(waiter, "attachment loopback status timed out"),
        ATTACHMENT_TRANSFER_STATUS_DEADLINE_MS,
      );
    }
    return waiter.promise;
  }

  close(reason: string): void {
    this.finish(reason);
  }

  private createSocket(url: string, protocol: string): WebSocket {
    if (this.dependencies.createSocket) return this.dependencies.createSocket(url, protocol);
    if (typeof WebSocket === "undefined") throw new Error("WebSocket is unavailable");
    return new WebSocket(url, protocol);
  }

  private socketUrl(): string {
    const url = new URL(ATTACHMENT_TRANSFER_LOOPBACK_PATH, this.options.door.origin);
    url.protocol = url.protocol === "https:" ? "wss:" : "ws:";
    return url.toString();
  }

  private sendHello(): void {
    const grant = this.options.grant;
    if (!this.sendFrame({
      case: "hello",
      value: create(AttachmentTransferHelloSchema, {
        grantId: grant.grantId,
        secret: grant.secret,
        tabId: grant.tabId,
        deviceFingerprint: grant.deviceFingerprint,
        sessionId: grant.sessionId,
        uploadId: grant.uploadId,
        filename: grant.filename,
        shortPath: grant.shortPath,
        totalBytes: BigInt(grant.totalBytes),
        peerId: "",
        workerEpoch: grant.workerEpoch,
      }),
    })) this.finish("attachment loopback could not authenticate");
  }

  private receive(data: unknown): void {
    if (this.closed || !(data instanceof ArrayBuffer)) return this.finish("attachment loopback received an invalid frame");
    try {
      const frame = fromBinary(AttachmentTransferServerFrameSchema, new Uint8Array(data)).frame;
      if (!this.ready) {
        if (frame.case !== "ready") return this.finish("attachment loopback required Ready first");
        return this.admitReady(frame.value);
      }
      if (frame.case === "ack") return this.receiveAck(frame.value);
      if (frame.case === "status") return this.receiveStatus(frame.value);
      if (frame.case === "closed") return this.finish("attachment loopback closed");
      this.finish("attachment loopback received an invalid frame");
    } catch {
      this.finish("attachment loopback received an invalid frame");
    }
  }

  private admitReady(ready: AttachmentTransferReady): void {
    const grant = this.options.grant;
    if (
      ready.workerFingerprint !== this.options.door.workerFingerprint
      || ready.workerFingerprint !== grant.workerFp
      || ready.workerEpoch !== grant.workerEpoch
      || ready.sessionId !== grant.sessionId
      || ready.uploadId !== grant.uploadId
    ) return this.finish("attachment loopback Ready did not match its authenticated tuple");
    this.ready = true;
    clearTimeout(this.setupTimer ?? undefined);
    this.setupTimer = null;
    this.readyDeferred.resolve();
  }

  private receiveAck(ack: { uploadId: string; seq: number; bytesReceived: bigint; absPath: string; error: string; chunkSha256: string }): void {
    const waiter = this.ackWaiter;
    if (!waiter && this.statusWaiter?.uploadId === ack.uploadId) return;
    const bytesReceived = Number(ack.bytesReceived);
    if (
      !waiter
      || ack.uploadId !== this.options.grant.uploadId
      || !Number.isSafeInteger(ack.seq)
      || ack.seq < 0
      || ack.seq !== waiter.seq
      || !Number.isSafeInteger(bytesReceived)
      || bytesReceived < 0
    ) return this.finish("attachment loopback acknowledged an unexpected chunk");
    if (ack.error) return this.rejectAck(waiter, "attachment loopback rejected a chunk");
    this.resolveAck(waiter, { bytesReceived, absPath: ack.absPath, chunkSha256: ack.chunkSha256 });
  }

  private receiveStatus(status: ProtocolAttachmentTransferStatus): void {
    const waiter = this.statusWaiter;
    const bytesReceived = Number(status.bytesReceived);
    if (
      !waiter
      || status.uploadId !== waiter.uploadId
      || !Number.isSafeInteger(status.nextSeq)
      || status.nextSeq < 0
      || !Number.isSafeInteger(bytesReceived)
      || bytesReceived < 0
    ) return this.finish("attachment loopback returned invalid status");
    this.resolveStatus(waiter, {
      uploadId: status.uploadId,
      nextSeq: status.nextSeq,
      bytesReceived,
      lastChunkSha256: status.lastChunkSha256,
      committed: status.committed,
      absPath: status.absPath,
      error: status.error,
    });
  }

  private sendFrame(frame: AttachmentTransferClientFrame["frame"]): boolean {
    const socket = this.socket;
    if (this.closed || !socket || socket.readyState !== 1) return false;
    try {
      socket.send(toBinary(AttachmentTransferClientFrameSchema, create(AttachmentTransferClientFrameSchema, { frame })));
      return true;
    } catch {
      return false;
    }
  }

  private finish(reason: string): void {
    if (this.closed) return;
    this.closed = true;
    clearTimeout(this.setupTimer ?? undefined);
    this.setupTimer = null;
    const socket = this.socket;
    this.socket = null;
    if (socket) {
      socket.onopen = null;
      socket.onmessage = null;
      socket.onclose = null;
      socket.onerror = null;
      try { socket.close(); } catch { /* already unusable */ }
    }
    const failure = new AttachmentTransferCarrierError(reason, this.sentChunk);
    if (!this.ready) this.readyDeferred.reject(failure);
    const ackWaiter = this.ackWaiter;
    if (ackWaiter) this.rejectAck(ackWaiter, reason, this.sentChunk);
    const statusWaiter = this.statusWaiter;
    if (statusWaiter) this.rejectStatus(statusWaiter, reason);
  }

  private resolveAck(waiter: AckWaiter, ack: AttachmentTransferAck): void {
    if (this.ackWaiter !== waiter) return;
    this.ackWaiter = null;
    clearTimeout(waiter.timer ?? undefined);
    waiter.resolve(ack);
  }

  private rejectAck(waiter: AckWaiter, reason: string, ambiguous = false): void {
    if (this.ackWaiter !== waiter) return;
    this.ackWaiter = null;
    clearTimeout(waiter.timer ?? undefined);
    waiter.reject(new AttachmentTransferCarrierError(reason, this.sentChunk, ambiguous));
  }

  private resolveStatus(waiter: StatusWaiter, status: AttachmentTransferStatus): void {
    if (this.statusWaiter !== waiter) return;
    this.statusWaiter = null;
    clearTimeout(waiter.timer ?? undefined);
    waiter.resolve(status);
  }

  private rejectStatus(waiter: StatusWaiter, reason: string): void {
    if (this.statusWaiter !== waiter) return;
    this.statusWaiter = null;
    clearTimeout(waiter.timer ?? undefined);
    waiter.reject(new AttachmentTransferCarrierError(reason, this.sentChunk));
  }
}

function deferred<T>(): Deferred<T> {
  let resolve!: (value: T) => void;
  let reject!: (error: Error) => void;
  const promise = new Promise<T>((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, resolve, reject };
}
