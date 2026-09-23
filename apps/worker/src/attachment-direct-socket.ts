// Direct attachment protobuf admission for loopback and authenticated WebRTC ports.
// A dedicated exact AttachmentGrantStore binds one descriptor before bytes reach
// the durable operation owner; terminal grant authority and terminal sockets stay separate.

import { fromBinary } from "@bufbuild/protobuf";
import {
  ATTACHMENT_TRANSFER_COMPLETE_REASON,
  ATTACHMENT_TRANSFER_DIRECT_CHUNK_BYTES,
  ATTACHMENT_TRANSFER_HELLO_DEADLINE_MS,
  ATTACHMENT_TRANSFER_MAX_ACTIVE_PER_WORKER,
  type AttachmentTransferErrorReason,
  type AttachmentTransferPeerChannelLane,
} from "@roost/shared/attachment-transfer";
import {
  AttachmentTransferClientFrameSchema,
  type AttachmentTransferChunk,
  type AttachmentTransferHello,
  type AttachmentTransferStatusRequest,
} from "@roost/shared/proto/attachment_transfer_pb";
import { log } from "@roost/shared/log";
import type {
  AttachmentGrantChange,
  AttachmentGrantStore,
} from "./attachment-grants.ts";
import {
  admitAttachmentTransferHello,
  attachmentMetadataMatchesGrant,
} from "./attachment-transfer-admission.ts";
import {
  attachmentOperationStatus,
  detachDirectAttachmentCarrier,
  handleDirectAttachmentChunk,
} from "./attachment-upload.ts";
import type { AttachmentOperationReceipt } from "./attachment-operation-receipts.ts";
import { holdsAdmittedSlot, type AttachmentPortSession } from "./attachment-direct-session.ts";
import {
  sendAttachmentAck,
  sendAttachmentClosed,
  sendAttachmentReady,
  sendAttachmentStatus,
} from "./attachment-direct-frames.ts";
import type { AttachmentPeerExpectedTuple } from "./attachment-peer-connection.ts";
import {
  type AttachmentPeerPacketIngress,
  type AttachmentPeerPacketPort,
} from "./attachment-peer-packet-port.ts";
import type { AttachmentTransferPort } from "./attachment-transfer-port.ts";
import { AttachmentTransferLease } from "./attachment-transfer-lease.ts";

export interface AttachmentDirectSocketsDeps {
  readonly grants: AttachmentGrantStore;
  readonly workerFingerprint: string;
  readonly workerEpoch: string;
}

/** One process-owned direct receiver; AttachmentOperationOwner owns file state. */
export class AttachmentDirectSockets {
  private readonly sessions = new Map<string, AttachmentPortSession>();
  private readonly unsubscribeGrant: () => void;
  private disposed = false;
  constructor(private readonly deps: AttachmentDirectSocketsDeps) {
    this.unsubscribeGrant = deps.grants.subscribe((change) => { this.handleGrantChange(change); });
  }
  onOpen(port: AttachmentTransferPort): void {
    // Unauthenticated loopback sockets fill only their own bucket, never an admitted upload's slot.
    if (this.disposed || this.countSessions((session) => !holdsAdmittedSlot(session)) >= ATTACHMENT_TRANSFER_MAX_ACTIVE_PER_WORKER) {
      port.close(1013, "attachment transfer capacity is full");
      return;
    }
    const session: AttachmentPortSession = {
      port,
      expectedPeer: null,
      metadata: null,
      terminal: false,
      setupTimer: undefined,
      lease: null,
      writePending: false,
    };
    this.sessions.set(port.socketId, session);
    this.armLoopbackHelloDeadline(session);
    log.info("attachment-transfer", "socket_opened", { carrier: "loopback", active: this.sessions.size });
  }
  onMessage(port: AttachmentTransferPort, bytes: Uint8Array): Promise<void> {
    return this.receive(port, "loopback", bytes);
  }
  onClose(port: AttachmentTransferPort): void {
    const session = this.sessions.get(port.socketId);
    if (!session) return;
    this.removeSession(session);
    if (session.metadata && !session.terminal) detachDirectAttachmentCarrier(port.socketId);
    log.info("attachment-transfer", "socket_closed", { carrier: port.kind, active: this.sessions.size });
  }
  revokeDevice(deviceFingerprint: string): void {
    for (const session of [...this.sessions.values()]) {
      if (session.metadata?.deviceFingerprint === deviceFingerprint) this.fail(session, "grant_unavailable", 0, "");
    }
  }
  openPeerPort(port: AttachmentPeerPacketPort, expectedPeer: AttachmentPeerExpectedTuple): AttachmentPeerPacketIngress {
    if (this.disposed || this.countSessions(holdsAdmittedSlot) >= ATTACHMENT_TRANSFER_MAX_ACTIVE_PER_WORKER) {
      port.close(1013, "attachment transfer capacity is full");
      return { onMessage: () => undefined };
    }
    this.sessions.set(port.socketId, {
      port,
      expectedPeer,
      metadata: null,
      terminal: false,
      setupTimer: undefined,
      lease: null,
      writePending: false,
    });
    log.info("attachment-transfer", "peer_port_opened", { active: this.sessions.size });
    return {
      onMessage: (lane, bytes) => { void this.receive(port, lane, bytes); },
      onClose: () => { this.onClose(port); },
    };
  }
  dispose(): void {
    if (this.disposed) return;
    this.disposed = true;
    this.unsubscribeGrant();
    for (const session of [...this.sessions.values()]) {
      this.removeSession(session);
      if (session.metadata && !session.terminal) detachDirectAttachmentCarrier(session.port.socketId);
      session.port.close(1001, "worker disposed");
    }
  }

  private async receive(
    port: AttachmentTransferPort,
    lane: AttachmentTransferPeerChannelLane | "loopback",
    bytes: Uint8Array,
  ): Promise<void> {
    const session = this.sessions.get(port.socketId);
    if (!session || session.terminal) return;
    let frame;
    try {
      frame = fromBinary(AttachmentTransferClientFrameSchema, bytes).frame;
    } catch {
      this.fail(session, "invalid_hello", 0, "");
      return;
    }
    if (!frame) {
      this.fail(session, "invalid_hello", 0, "");
      return;
    }
    if (session.metadata === null) {
      if (frame.case !== "hello" || (lane !== "loopback" && lane !== "control")) {
        this.fail(session, "invalid_hello", 0, "");
        return;
      }
      this.acceptHello(session, frame.value);
      return;
    }
    if (session.writePending) {
      this.fail(session, "chunk_out_of_order", 0, "");
      return;
    }
    if (frame.case === "statusRequest") {
      if (lane !== "loopback" && lane !== "control") {
        this.fail(session, "upload_mismatch", 0, "");
        return;
      }
      this.acceptStatusRequest(session, frame.value);
      return;
    }
    if (frame.case !== "chunk" || (lane !== "loopback" && lane !== "data")) {
      this.fail(session, "upload_mismatch", 0, "");
      return;
    }
    await this.acceptChunk(session, frame.value);
  }

  private acceptHello(session: AttachmentPortSession, hello: AttachmentTransferHello): void {
    const admission = admitAttachmentTransferHello(hello, {
      grants: this.deps.grants,
      expectedPeer: session.expectedPeer,
      workerEpoch: this.deps.workerEpoch,
    });
    if (!admission.ok) {
      this.fail(session, admission.reason, 0, "");
      return;
    }
    const grantId = admission.metadata.grantId;
    if ([...this.sessions.values()].some((other) => other !== session && other.metadata?.grantId === grantId)) {
      // One grant authorizes one live carrier; a replay can neither multiply slots nor steal the upload.
      this.fail(session, "grant_unavailable", 0, "");
      return;
    }
    if (
      session.expectedPeer === null
      && this.countSessions(holdsAdmittedSlot) >= ATTACHMENT_TRANSFER_MAX_ACTIVE_PER_WORKER
    ) {
      session.terminal = true;
      this.removeSession(session);
      session.port.close(1013, "attachment transfer capacity is full");
      return;
    }
    session.metadata = admission.metadata;
    clearTimeout(session.setupTimer);
    session.setupTimer = undefined;
    if (!sendAttachmentReady(session.port, {
      workerFingerprint: this.deps.workerFingerprint,
      workerEpoch: this.deps.workerEpoch,
      sessionId: admission.metadata.sessionId,
      uploadId: admission.metadata.uploadId,
    })) {
      this.retireUnacknowledgedRoute(session, "write_failed");
      return;
    }
    session.port.markAuthenticated();
    session.lease = new AttachmentTransferLease({
      onExpired: () => { this.fail(session, "grant_unavailable", 0, ""); },
    });
    session.lease.start();
    log.info("attachment-transfer", "socket_authenticated", {
      carrier: session.port.kind,
      active: this.sessions.size,
    });
  }

  private async acceptChunk(session: AttachmentPortSession, chunk: AttachmentTransferChunk): Promise<void> {
    const metadata = session.metadata;
    const offset = chunk.offset <= BigInt(Number.MAX_SAFE_INTEGER) ? Number(chunk.offset) : null;
    if (!metadata) {
      this.fail(session, "grant_unavailable", chunk.seq, chunk.chunkSha256);
      return;
    }
    if (!session.lease?.allowsActivity()) return;
    if (chunk.uploadId !== metadata.uploadId) {
      this.fail(session, "upload_mismatch", chunk.seq, chunk.chunkSha256);
      return;
    }
    if (offset === null) {
      this.fail(session, "chunk_offset_mismatch", chunk.seq, chunk.chunkSha256);
      return;
    }
    if (chunk.data.byteLength > ATTACHMENT_TRANSFER_DIRECT_CHUNK_BYTES) {
      this.fail(session, "chunk_too_large", chunk.seq, chunk.chunkSha256);
      return;
    }
    session.writePending = true;
    try {
      await handleDirectAttachmentChunk({
        request_id: metadata.uploadId,
        session_id: metadata.sessionId,
        filename: metadata.filename,
        short_path: metadata.shortPath,
        total_bytes: metadata.totalBytes,
        data: chunk.data,
        last: chunk.last,
        seq: chunk.seq,
        offset,
        chunk_sha256: chunk.chunkSha256,
        carrier: "direct",
        carrier_id: session.port.socketId,
      }, {
        progress: (receipt) => { this.acknowledgeProgress(session, receipt); },
        ok: (absPath, receipt) => { this.acknowledgeCompletion(session, receipt, absPath); },
        err: (_message, error) => { this.fail(session, error ?? "write_failed", chunk.seq, chunk.chunkSha256); },
      });
    } finally {
      session.writePending = false;
    }
  }

  private acceptStatusRequest(session: AttachmentPortSession, request: AttachmentTransferStatusRequest): void {
    const metadata = session.metadata;
    if (!metadata || request.uploadId !== metadata.uploadId) {
      this.fail(session, "upload_mismatch", 0, "");
      return;
    }
    // Status is lost-ACK recovery, not progress: it must not keep an idle port alive.
    if (!session.lease?.allowsActivity()) return;
    if (!sendAttachmentStatus(session.port, attachmentOperationStatus(metadata.sessionId, metadata.uploadId))) {
      this.retireUnacknowledgedRoute(session, "write_failed");
    }
  }

  private acknowledgeProgress(session: AttachmentPortSession, receipt: AttachmentOperationReceipt): void {
    if (!session.lease?.noteValidActivity()) return;
    if (!this.sendAck(session, receipt, "")) this.retireUnacknowledgedRoute(session, "write_failed");
  }

  private acknowledgeCompletion(
    session: AttachmentPortSession,
    receipt: AttachmentOperationReceipt,
    absPath: string,
  ): void {
    if (!session.lease?.noteValidActivity()) return;
    if (!this.sendAck(session, receipt, absPath)) {
      this.closeAfterTerminalFrame(session, "write_failed");
      return;
    }
    session.terminal = true;
    sendAttachmentClosed(session.port, ATTACHMENT_TRANSFER_COMPLETE_REASON);
    this.closeAfterTerminalFrame(session, ATTACHMENT_TRANSFER_COMPLETE_REASON);
    log.info("attachment-transfer", "upload_completed", { carrier: session.port.kind });
  }

  private fail(
    session: AttachmentPortSession,
    reason: AttachmentTransferErrorReason,
    seq: number,
    chunkSha256: string,
  ): void {
    if (session.terminal) return;
    session.terminal = true;
    const metadata = session.metadata;
    if (metadata) {
      const bytesReceived = attachmentOperationStatus(metadata.sessionId, metadata.uploadId).bytesReceived;
      sendAttachmentAck(session.port, metadata.uploadId, { seq, bytesReceived, chunkSha256 }, "", reason);
      detachDirectAttachmentCarrier(session.port.socketId);
    }
    sendAttachmentClosed(session.port, reason);
    this.closeAfterTerminalFrame(session, reason);
    log.info("attachment-transfer", "upload_failed", { carrier: session.port.kind, reason });
  }

  private sendAck(session: AttachmentPortSession, receipt: AttachmentOperationReceipt, absPath: string): boolean {
    return session.metadata !== null && sendAttachmentAck(session.port, session.metadata.uploadId, receipt, absPath, "");
  }

  private retireUnacknowledgedRoute(session: AttachmentPortSession, reason: string): void {
    session.terminal = true;
    if (session.metadata) detachDirectAttachmentCarrier(session.port.socketId);
    this.closeAfterTerminalFrame(session, reason);
  }

  private closeAfterTerminalFrame(session: AttachmentPortSession, reason: string): void {
    this.removeSession(session);
    session.port.closeAfterDrain(reason);
  }

  private countSessions(predicate: (session: AttachmentPortSession) => boolean): number {
    let count = 0;
    for (const session of this.sessions.values()) if (predicate(session)) count += 1;
    return count;
  }

  private removeSession(session: AttachmentPortSession): void {
    if (this.sessions.get(session.port.socketId) !== session) return;
    this.sessions.delete(session.port.socketId);
    clearTimeout(session.setupTimer);
    session.lease?.dispose();
    session.lease = null;
    session.setupTimer = undefined;
  }

  private armLoopbackHelloDeadline(session: AttachmentPortSession): void {
    if (session.port.kind !== "loopback") return;
    session.setupTimer = setTimeout(() => {
      session.setupTimer = undefined;
      if (session.metadata === null) this.fail(session, "invalid_hello", 0, "");
    }, ATTACHMENT_TRANSFER_HELLO_DEADLINE_MS);
    session.setupTimer.unref?.();
  }

  private handleGrantChange(change: AttachmentGrantChange): void {
    if (change.kind === "removed" && change.reason === "expired") return;
    for (const session of [...this.sessions.values()]) {
      const metadata = session.metadata;
      if (!metadata || metadata.grantId !== change.grant.grantId) continue;
      if (change.kind === "removed" || !attachmentMetadataMatchesGrant(metadata, change.grant)) {
        this.fail(session, "grant_unavailable", 0, "");
      }
    }
  }
}
