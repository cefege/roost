// One negotiated WebRTC transport for one direct attachment upload.
// attachmentDirect opens it only after loopback is unavailable before chunk zero.
// It uses attachment-only packet queues on separate ordered control and data channels.

import { create, fromBinary, toBinary } from "@bufbuild/protobuf";
import {
  ATTACHMENT_TRANSFER_ACK_DEADLINE_MS,
  ATTACHMENT_TRANSFER_HELLO_DEADLINE_MS,
  ATTACHMENT_TRANSFER_PEER_DATA_CHANNELS,
  ATTACHMENT_TRANSFER_STATUS_DEADLINE_MS,
  type AttachmentTransferPeerChannelLane,
} from "@roost/shared/attachment-transfer";
import {
  AttachmentTransferChunkSchema,
  AttachmentTransferClientFrameSchema,
  AttachmentTransferHelloSchema,
  AttachmentTransferServerFrameSchema,
  AttachmentTransferStatusRequestSchema,
  type AttachmentTransferClientFrame,
  type AttachmentTransferReady,
  type AttachmentTransferServerFrame,
  type AttachmentTransferStatus as ProtocolAttachmentTransferStatus,
} from "@roost/shared/proto/attachment_transfer_pb";
import type { AttachmentDirectGrant } from "./attachmentDirectGrant.ts";
import { AttachmentPeerPacketLanes } from "./attachmentPeerPackets.ts";
import {
  AttachmentPeerSignaling,
  type AttachmentPeerSignalingDependencies,
} from "./attachmentPeerSignaling.ts";
import { terminalDirectBufferSource } from "../ws/terminal-direct-browser.ts";
import {
  AttachmentTransferCarrierError,
  type AttachmentTransferAck,
  type AttachmentTransferChunk,
  type AttachmentTransferConnection,
  type AttachmentTransferStatus,
} from "./attachmentTransfer.ts";

export interface AttachmentPeerTransferOptions { readonly grant: AttachmentDirectGrant; readonly peerId: string; }
export type AttachmentPeerTransferDependencies = AttachmentPeerSignalingDependencies;

interface Deferred<T> { readonly promise: Promise<T>; readonly resolve: (value: T) => void; readonly reject: (error: Error) => void; }
interface TimedWaiter<T> extends Deferred<T> { timer: ReturnType<typeof setTimeout> | null; }
interface AckWaiter extends TimedWaiter<AttachmentTransferAck> { readonly seq: number; }
interface StatusWaiter extends TimedWaiter<AttachmentTransferStatus> { readonly uploadId: string; }

/** Opens a new attachment peer; it is never shared with terminal transport. */
export function openAttachmentPeerTransfer(
  options: AttachmentPeerTransferOptions,
  dependencies: AttachmentPeerTransferDependencies = {},
): Promise<AttachmentTransferConnection> {
  try {
    return new AttachmentPeerTransfer(options, dependencies).open();
  } catch {
    return Promise.reject(new AttachmentTransferCarrierError("attachment peer could not open", false));
  }
}

class AttachmentPeerTransfer implements AttachmentTransferConnection {
  private readonly readyDeferred = deferred<void>();
  private readonly signaling: AttachmentPeerSignaling;
  private readonly peerConnection: RTCPeerConnection;
  private readonly packets: AttachmentPeerPacketLanes;
  private readonly channels: Record<AttachmentTransferPeerChannelLane, RTCDataChannel | null> = { control: null, data: null };
  private helloTimer: ReturnType<typeof setTimeout> | null = null;
  private ackWaiter: AckWaiter | null = null;
  private statusWaiter: StatusWaiter | null = null;
  private flushScheduled = false;
  private readyFrameReceived = false;
  private authenticated = false;
  private closed = false;
  sentChunk = false;

  constructor(
    private readonly options: AttachmentPeerTransferOptions,
    dependencies: AttachmentPeerTransferDependencies,
  ) {
    this.signaling = new AttachmentPeerSignaling(options.grant.stunUrls, dependencies);
    this.peerConnection = this.signaling.peerConnection;
    this.packets = new AttachmentPeerPacketLanes(() => this.finish("attachment peer packet stalled"));
    try {
      this.installPeerHandlers();
      this.createStaticChannels();
    } catch {
      this.signaling.close();
      throw new Error("attachment peer could not initialize");
    }
    void this.readyDeferred.promise.catch(() => undefined);
  }

  async open(): Promise<AttachmentTransferConnection> {
    try {
      await this.signaling.negotiate(this.options.grant, this.options.peerId);
      return await this.readyDeferred.promise.then(() => this);
    } catch {
      this.finish("attachment peer setup failed");
      throw new AttachmentTransferCarrierError("attachment peer setup failed", this.sentChunk);
    }
  }

  sendChunk(chunk: AttachmentTransferChunk): Promise<AttachmentTransferAck> {
    if (this.closed || !this.authenticated || this.ackWaiter !== null) {
      return Promise.reject(new AttachmentTransferCarrierError("attachment peer cannot send a chunk", this.sentChunk));
    }
    const waiter: AckWaiter = { ...deferred<AttachmentTransferAck>(), seq: chunk.seq, timer: null };
    this.ackWaiter = waiter;
    if (!this.queueFrame("data", { case: "chunk", value: create(AttachmentTransferChunkSchema, chunk) })) {
      this.rejectAck(waiter, "attachment peer could not queue a chunk");
    }
    return waiter.promise;
  }

  requestStatus(uploadId: string): Promise<AttachmentTransferStatus> {
    if (this.closed || !this.authenticated || this.statusWaiter !== null) {
      return Promise.reject(new AttachmentTransferCarrierError("attachment peer cannot request status", this.sentChunk));
    }
    const waiter: StatusWaiter = { ...deferred<AttachmentTransferStatus>(), uploadId, timer: null };
    this.statusWaiter = waiter;
    if (!this.queueFrame("control", { case: "statusRequest", value: create(AttachmentTransferStatusRequestSchema, { uploadId }) })) {
      this.rejectStatus(waiter, "attachment peer could not request status");
    } else if (this.statusWaiter === waiter) {
      waiter.timer = setTimeout(() => this.rejectStatus(waiter, "attachment peer status timed out"), ATTACHMENT_TRANSFER_STATUS_DEADLINE_MS);
    }
    return waiter.promise;
  }

  close(reason: string): void { this.finish(reason); }

  private installPeerHandlers(): void {
    this.peerConnection.ondatachannel = () => this.finish("attachment peer offered an unsolicited data channel");
    this.peerConnection.ontrack = () => this.finish("attachment peer offered a media track");
    this.peerConnection.onconnectionstatechange = () => {
      if (["failed", "closed"].includes(this.peerConnection.connectionState)) this.finish("attachment peer connection failed");
    };
    this.peerConnection.oniceconnectionstatechange = () => {
      if (["failed", "closed"].includes(this.peerConnection.iceConnectionState)) this.finish("attachment peer ICE failed");
    };
  }

  private createStaticChannels(): void {
    for (const definition of ATTACHMENT_TRANSFER_PEER_DATA_CHANNELS) {
      const channel = this.peerConnection.createDataChannel(definition.label, {
        negotiated: true, id: definition.id, ordered: definition.ordered, protocol: definition.protocol,
      });
      channel.binaryType = "arraybuffer";
      channel.onopen = () => this.openChannel(definition.lane);
      channel.onclose = () => this.finish("attachment peer channel closed");
      channel.onerror = () => this.finish("attachment peer channel failed");
      channel.onbufferedamountlow = () => this.scheduleFlush();
      channel.onmessage = (event) => this.receivePacket(definition.lane, event.data);
      this.channels[definition.lane] = channel;
    }
  }

  private openChannel(lane: AttachmentTransferPeerChannelLane): void {
    if (this.closed) return;
    if (lane === "control") {
      this.helloTimer = setTimeout(() => this.finish("attachment peer hello timed out"), ATTACHMENT_TRANSFER_HELLO_DEADLINE_MS);
      const grant = this.options.grant;
      if (!this.queueFrame("control", {
        case: "hello",
        value: create(AttachmentTransferHelloSchema, {
          grantId: grant.grantId, secret: grant.secret, tabId: grant.tabId, deviceFingerprint: grant.deviceFingerprint,
          sessionId: grant.sessionId, uploadId: grant.uploadId, filename: grant.filename, shortPath: grant.shortPath,
          totalBytes: BigInt(grant.totalBytes), peerId: this.options.peerId, workerEpoch: grant.workerEpoch,
        }),
      })) this.finish("attachment peer could not authenticate");
    }
    this.admitWhenChannelsOpen();
  }

  private queueFrame(lane: AttachmentTransferPeerChannelLane, frame: AttachmentTransferClientFrame["frame"]): boolean {
    const channel = this.channels[lane];
    if (this.closed || !channel || channel.readyState !== "open") return false;
    try {
      if (!this.packets.enqueue(lane, toBinary(AttachmentTransferClientFrameSchema, create(AttachmentTransferClientFrameSchema, { frame })))) return false;
      this.scheduleFlush();
      return true;
    } catch {
      return false;
    }
  }

  private scheduleFlush(): void {
    if (this.closed || this.flushScheduled) return;
    this.flushScheduled = true;
    queueMicrotask(() => this.flushPackets());
  }

  private flushPackets(): void {
    this.flushScheduled = false;
    if (this.closed) return;
    let sentAny = false;
    for (const definition of ATTACHMENT_TRANSFER_PEER_DATA_CHANNELS) {
      const lane = definition.lane;
      const channel = this.channels[lane];
      const fragment = channel?.readyState === "open" ? this.packets.nextFragment(lane) : null;
      if (!channel || !fragment) continue;
      try {
        channel.send(terminalDirectBufferSource(fragment.bytes));
        if (lane === "data") this.sentChunk = true;
        fragment.commit();
        if (lane === "data" && fragment.final && this.ackWaiter?.timer === null) {
          const waiter = this.ackWaiter;
          waiter.timer = setTimeout(() => this.rejectAck(waiter, "attachment peer acknowledgement timed out", true), ATTACHMENT_TRANSFER_ACK_DEADLINE_MS);
        }
        sentAny = true;
      } catch {
        this.finish("attachment peer data channel send failed");
        return;
      }
    }
    if (sentAny && this.packets.hasQueuedPackets()) this.scheduleFlush();
  }

  private receivePacket(lane: AttachmentTransferPeerChannelLane, data: unknown): void {
    if (this.closed || !(data instanceof ArrayBuffer)) return this.finish("attachment peer received an invalid packet");
    try {
      const message = this.packets.receive(lane, new Uint8Array(data));
      if (message) this.receiveFrame(lane, fromBinary(AttachmentTransferServerFrameSchema, message).frame);
    } catch {
      this.finish("attachment peer received an invalid packet");
    }
  }

  private receiveFrame(lane: AttachmentTransferPeerChannelLane, frame: AttachmentTransferServerFrame["frame"]): void {
    if (!this.readyFrameReceived) {
      if (lane !== "control" || frame.case !== "ready") return this.finish("attachment peer required Ready first");
      return this.admitReady(frame.value);
    }
    if (!this.authenticated || lane !== "control" || frame.case === "ready") return this.finish("attachment peer received an invalid frame");
    if (frame.case === "ack") return this.receiveAck(frame.value);
    if (frame.case === "status") return this.receiveStatus(frame.value);
    if (frame.case === "closed") return this.finish("attachment peer closed");
    this.finish("attachment peer received an invalid frame");
  }

  private admitReady(ready: AttachmentTransferReady): void {
    const grant = this.options.grant;
    if (ready.workerFingerprint !== grant.workerFp || ready.workerEpoch !== grant.workerEpoch || ready.sessionId !== grant.sessionId || ready.uploadId !== grant.uploadId) {
      return this.finish("attachment peer Ready did not match its authenticated tuple");
    }
    this.readyFrameReceived = true;
    this.admitWhenChannelsOpen();
  }

  private admitWhenChannelsOpen(): void {
    if (!this.readyFrameReceived || this.authenticated || ATTACHMENT_TRANSFER_PEER_DATA_CHANNELS.some((definition) => this.channels[definition.lane]?.readyState !== "open")) return;
    this.authenticated = true;
    clearTimeout(this.helloTimer ?? undefined);
    this.helloTimer = null;
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
    ) {
      return this.finish("attachment peer acknowledged an unexpected chunk");
    }
    if (ack.error) return this.rejectAck(waiter, "attachment peer rejected a chunk");
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
    ) {
      return this.finish("attachment peer returned invalid status");
    }
    this.resolveStatus(waiter, {
      uploadId: status.uploadId, nextSeq: status.nextSeq, bytesReceived, lastChunkSha256: status.lastChunkSha256,
      committed: status.committed, absPath: status.absPath, error: status.error,
    });
  }

  private finish(reason: string): void {
    if (this.closed) return;
    this.closed = true;
    clearTimeout(this.helloTimer ?? undefined);
    this.helloTimer = null;
    this.packets.clear();
    const failure = new AttachmentTransferCarrierError(reason, this.sentChunk);
    if (!this.authenticated) this.readyDeferred.reject(failure);
    const ackWaiter = this.ackWaiter;
    if (ackWaiter) this.rejectAck(ackWaiter, reason, this.sentChunk);
    const statusWaiter = this.statusWaiter;
    if (statusWaiter) this.rejectStatus(statusWaiter, reason);
    for (const definition of ATTACHMENT_TRANSFER_PEER_DATA_CHANNELS) {
      const channel = this.channels[definition.lane];
      if (!channel) continue;
      channel.onopen = null;
      channel.onclose = null;
      channel.onerror = null;
      channel.onbufferedamountlow = null;
      channel.onmessage = null;
      try { channel.close(); } catch { /* already unusable */ }
    }
    this.peerConnection.ondatachannel = null;
    this.peerConnection.ontrack = null;
    this.peerConnection.onconnectionstatechange = null;
    this.peerConnection.oniceconnectionstatechange = null;
    this.signaling.close();
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
