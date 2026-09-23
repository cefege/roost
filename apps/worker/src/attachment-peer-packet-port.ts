// Fragmented WebRTC byte carrier for one direct attachment upload.
// It owns attachment-only packet queues and reassembly; AttachmentDirectSockets
// remains the sole protobuf admission boundary and never sends raw large frames.

import {
  ATTACHMENT_TRANSFER_ACK_DEADLINE_MS,
  ATTACHMENT_TRANSFER_HELLO_DEADLINE_MS,
  ATTACHMENT_TRANSFER_PACKET_MAX_BYTES,
  ATTACHMENT_TRANSFER_PACKET_STALL_MS,
  type AttachmentTransferPeerChannelLane,
} from "@roost/shared/attachment-transfer";
import {
  AttachmentTransferPacketAssembler,
  AttachmentTransferPacketQueue,
  parseAttachmentTransferPacket,
} from "@roost/shared/attachment-transfer-packets";
import type {
  AttachmentTransferPort,
  AttachmentTransferSendResult,
} from "./attachment-transfer-port.ts";
import type { AttachmentPeerPacketPeerBudget } from "./attachment-peer-packet-budget.ts";

export interface AttachmentPeerNativeDataChannel {
  close(): void;
  sendMessageBinary(bytes: Buffer | Uint8Array): boolean;
  isOpen(): boolean;
  bufferedAmount(): number;
  setBufferedAmountLowThreshold(bytes: number): void;
  onOpen(callback: () => void): void;
  onClosed(callback: () => void): void;
  onError(callback: (error: string) => void): void;
  onBufferedAmountLow(callback: () => void): void;
  onMessage(callback: (message: string | Uint8Array | ArrayBuffer) => void): void;
}

export type AttachmentPeerNativeDataChannels = Record<
  AttachmentTransferPeerChannelLane,
  AttachmentPeerNativeDataChannel
>;

export interface AttachmentPeerPacketIngress {
  onMessage(lane: AttachmentTransferPeerChannelLane, bytes: Uint8Array): void;
  onClose?(): void;
}

export interface AttachmentPeerPacketPortDeps {
  readonly socketId: string;
  readonly channels: AttachmentPeerNativeDataChannels;
  readonly packetBudget: AttachmentPeerPacketPeerBudget;
  readonly onClosed?: (reason: string) => void;
  readonly now?: () => number;
}

/** Attachment-only two-channel packet transport. */
export class AttachmentPeerPacketPort implements AttachmentTransferPort {
  readonly kind = "webrtc" as const;
  readonly socketId: string;
  private readonly queues: Record<AttachmentTransferPeerChannelLane, AttachmentTransferPacketQueue>;
  private readonly assemblers: Record<AttachmentTransferPeerChannelLane, AttachmentTransferPacketAssembler>;
  private readonly partialTimers: Record<AttachmentTransferPeerChannelLane, NodeJS.Timeout | undefined> = {
    control: undefined,
    data: undefined,
  };
  private readonly now: () => number;
  private ingress: AttachmentPeerPacketIngress | null = null;
  private authenticated = false;
  private closed = false;
  private flushing = false;
  private helloTimer: NodeJS.Timeout | undefined;
  private drainTimer: NodeJS.Timeout | undefined;
  private closeWhenDrained: string | undefined;

  constructor(private readonly deps: AttachmentPeerPacketPortDeps) {
    this.socketId = deps.socketId;
    this.now = deps.now ?? (() => performance.now());
    this.queues = {
      control: new AttachmentTransferPacketQueue("outgoing", deps.packetBudget.control),
      data: new AttachmentTransferPacketQueue("outgoing", deps.packetBudget.data),
    };
    this.assemblers = {
      control: new AttachmentTransferPacketAssembler("incoming", deps.packetBudget.control, this.now),
      data: new AttachmentTransferPacketAssembler("incoming", deps.packetBudget.data, this.now),
    };
    this.installChannelCallbacks("control");
    this.installChannelCallbacks("data");
  }

  get open(): boolean {
    return !this.closed && this.deps.channels.control.isOpen();
  }

  attachIngress(ingress: AttachmentPeerPacketIngress): void {
    if (this.closed || this.ingress !== null) throw new Error("attachment peer ingress is unavailable");
    this.ingress = ingress;
  }

  markAuthenticated(): void {
    if (this.closed || this.ingress === null) return;
    this.authenticated = true;
    clearTimeout(this.helloTimer);
    this.helloTimer = undefined;
  }

  send(bytes: Uint8Array, lane: AttachmentTransferPeerChannelLane): AttachmentTransferSendResult {
    if (this.closed) return "refused";
    const queue = this.queues[lane];
    const channel = this.deps.channels[lane];
    const queuedBefore = queue.queuedBytes;
    try {
      if (!queue.enqueue(bytes)) return "refused";
    } catch {
      return "refused";
    }
    const wasBackpressured = queuedBefore > 0 || !channel.isOpen() || channel.bufferedAmount() > 0;
    this.flush();
    return this.closed ? "refused" : wasBackpressured ? "backpressured" : "accepted";
  }

  closeAfterDrain(reason: string): void {
    if (this.closed) return;
    this.closeWhenDrained = reason;
    this.flush();
    this.closeIfDrained();
    if (this.closed || this.drainTimer !== undefined) return;
    this.drainTimer = setTimeout(() => {
      this.drainTimer = undefined;
      this.close(undefined, "ack_timeout");
    }, ATTACHMENT_TRANSFER_ACK_DEADLINE_MS);
    this.drainTimer.unref?.();
  }

  close(_code?: number, reason = "attachment peer port closed"): void {
    if (this.closed) return;
    this.closed = true;
    clearTimeout(this.helloTimer);
    clearTimeout(this.drainTimer);
    this.helloTimer = undefined;
    this.drainTimer = undefined;
    for (const lane of ["control", "data"] as const) {
      clearTimeout(this.partialTimers[lane]);
      this.partialTimers[lane] = undefined;
      try { this.queues[lane].clear(); } catch { /* quota cleanup remains local */ }
      try { this.assemblers[lane].reset(); } catch { /* malformed peer is closing */ }
      try { this.deps.channels[lane].close(); } catch { /* native channel already closed */ }
    }
    this.deps.packetBudget.dispose();
    try { this.ingress?.onClose?.(); } catch { /* close cannot escape native callbacks */ }
    this.deps.onClosed?.(reason);
  }

  private installChannelCallbacks(lane: AttachmentTransferPeerChannelLane): void {
    const channel = this.deps.channels[lane];
    channel.setBufferedAmountLowThreshold(0);
    channel.onOpen(() => {
      if (this.closed) return;
      if (lane === "control" && !this.authenticated && this.helloTimer === undefined) {
        this.helloTimer = setTimeout(() => {
          this.helloTimer = undefined;
          if (!this.authenticated) this.close(undefined, "hello_timeout");
        }, ATTACHMENT_TRANSFER_HELLO_DEADLINE_MS);
        this.helloTimer.unref?.();
      }
      this.flush();
    });
    channel.onBufferedAmountLow(() => { this.flush(); });
    channel.onMessage((message) => { this.receive(lane, message); });
    channel.onError(() => { this.close(undefined, "data_channel_error"); });
    channel.onClosed(() => { this.close(undefined, "data_channel_closed"); });
  }

  private receive(lane: AttachmentTransferPeerChannelLane, message: string | Uint8Array | ArrayBuffer): void {
    if (this.closed || typeof message === "string" || this.ingress === null) {
      this.close(undefined, "unexpected_client_data");
      return;
    }
    if (!this.authenticated && lane !== "control") {
      this.close(undefined, "unauthenticated_client_data");
      return;
    }
    const bytes = message instanceof ArrayBuffer
      ? new Uint8Array(message)
      : new Uint8Array(message.buffer, message.byteOffset, message.byteLength);
    if (bytes.byteLength > ATTACHMENT_TRANSFER_PACKET_MAX_BYTES) {
      this.close(undefined, "packet_too_large");
      return;
    }
    try {
      if (!this.authenticated && parseAttachmentTransferPacket(bytes).totalBytes > ATTACHMENT_TRANSFER_PACKET_MAX_BYTES) {
        this.close(undefined, "unauthenticated_frame_too_large");
        return;
      }
      const complete = this.assemblers[lane].push(bytes, this.now());
      this.armPartialDeadline(lane);
      if (complete !== null) this.ingress.onMessage(lane, complete);
    } catch {
      this.close(undefined, "packet_rejected");
    }
  }

  private armPartialDeadline(lane: AttachmentTransferPeerChannelLane): void {
    clearTimeout(this.partialTimers[lane]);
    this.partialTimers[lane] = undefined;
    if (!this.assemblers[lane].hasPartialMessage) return;
    this.partialTimers[lane] = setTimeout(() => {
      this.partialTimers[lane] = undefined;
      try {
        if (this.assemblers[lane].expire(this.now())) this.close(undefined, "packet_stalled");
      } catch {
        this.close(undefined, "packet_stalled");
      }
    }, ATTACHMENT_TRANSFER_PACKET_STALL_MS);
    this.partialTimers[lane]?.unref?.();
  }

  private flush(): void {
    if (this.closed || this.flushing) return;
    this.flushing = true;
    try {
      for (const lane of ["control", "data"] as const) {
        const channel = this.deps.channels[lane];
        while (!this.closed && channel.isOpen()) {
          const fragment = this.queues[lane].nextFragment();
          if (!fragment) break;
          try {
            // A false native result is buffered acceptance; commit this fragment once.
            channel.sendMessageBinary(Buffer.from(fragment.bytes.buffer, fragment.bytes.byteOffset, fragment.bytes.byteLength));
            fragment.commit();
          } catch {
            this.close(undefined, "native_send_failed");
            return;
          }
        }
      }
    } finally {
      this.flushing = false;
    }
    this.closeIfDrained();
  }

  private closeIfDrained(): void {
    if (
      !this.closed
      && this.closeWhenDrained !== undefined
      && this.queues.control.messageCount === 0
      && this.queues.data.messageCount === 0
      && this.nativeBuffersDrained()
    ) {
      this.close(undefined, this.closeWhenDrained);
    }
  }

  private nativeBuffersDrained(): boolean {
    try {
      return this.deps.channels.control.bufferedAmount() === 0 && this.deps.channels.data.bufferedAmount() === 0;
    } catch {
      return false;
    }
  }
}
