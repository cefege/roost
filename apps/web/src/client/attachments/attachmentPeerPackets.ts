// Packet framing, quotas, and reassembly for attachment WebRTC's two ordered channels.
// attachmentPeer owns the RTCPeerConnection and calls this owner for each native packet.
// Control and data lanes retain independent incoming and outgoing byte budgets.

import {
  ATTACHMENT_TRANSFER_PACKET_STALL_MS,
  ATTACHMENT_TRANSFER_PEER_CONTROL_QUEUE_MAX_BYTES,
  ATTACHMENT_TRANSFER_PEER_DATA_CHANNELS,
  ATTACHMENT_TRANSFER_PEER_DATA_QUEUE_MAX_BYTES,
  type AttachmentTransferPeerChannelLane,
} from "@roost/protocol/attachment-transfer";
import {
  AttachmentTransferPacketAssembler,
  AttachmentTransferPacketQueue,
  type AttachmentTransferPacketDirection,
  type AttachmentTransferPacketQueueFragment,
  type AttachmentTransferPacketQuota,
} from "@roost/protocol/attachment-transfer-packets";

type PacketQueueByLane = Record<AttachmentTransferPeerChannelLane, AttachmentTransferPacketQueue>;
type PacketAssemblerByLane = Record<AttachmentTransferPeerChannelLane, AttachmentTransferPacketAssembler>;
type PacketTimerByLane = Record<AttachmentTransferPeerChannelLane, ReturnType<typeof setTimeout> | null>;

/** Owns per-lane packet state so attachment traffic cannot borrow terminal queues. */
export class AttachmentPeerPacketLanes {
  private readonly outbound: PacketQueueByLane;
  private readonly inbound: PacketAssemblerByLane;
  private readonly stalls: PacketTimerByLane = { control: null, data: null };

  constructor(private readonly onStall: (lane: AttachmentTransferPeerChannelLane) => void) {
    const controlQuota = new AttachmentPeerPacketQuota(ATTACHMENT_TRANSFER_PEER_CONTROL_QUEUE_MAX_BYTES);
    const dataQuota = new AttachmentPeerPacketQuota(ATTACHMENT_TRANSFER_PEER_DATA_QUEUE_MAX_BYTES);
    const quotas: Record<AttachmentTransferPeerChannelLane, AttachmentPeerPacketQuota> = {
      control: controlQuota,
      data: dataQuota,
    };
    this.outbound = {
      control: new AttachmentTransferPacketQueue("outgoing", quotas.control),
      data: new AttachmentTransferPacketQueue("outgoing", quotas.data),
    };
    this.inbound = {
      control: new AttachmentTransferPacketAssembler("incoming", quotas.control),
      data: new AttachmentTransferPacketAssembler("incoming", quotas.data),
    };
  }

  enqueue(lane: AttachmentTransferPeerChannelLane, bytes: Uint8Array): boolean {
    return this.outbound[lane].enqueue(bytes);
  }

  nextFragment(lane: AttachmentTransferPeerChannelLane): AttachmentTransferPacketQueueFragment | null {
    return this.outbound[lane].nextFragment();
  }

  hasQueuedPackets(): boolean {
    return ATTACHMENT_TRANSFER_PEER_DATA_CHANNELS.some((definition) => this.outbound[definition.lane].messageCount > 0);
  }

  receive(lane: AttachmentTransferPeerChannelLane, bytes: Uint8Array): Uint8Array | null {
    const message = this.inbound[lane].push(bytes);
    this.armStall(lane);
    return message;
  }

  clear(): void {
    for (const definition of ATTACHMENT_TRANSFER_PEER_DATA_CHANNELS) {
      const lane = definition.lane;
      clearTimeout(this.stalls[lane] ?? undefined);
      this.stalls[lane] = null;
      this.outbound[lane].clear();
      this.inbound[lane].reset();
    }
  }

  private armStall(lane: AttachmentTransferPeerChannelLane): void {
    clearTimeout(this.stalls[lane] ?? undefined);
    this.stalls[lane] = null;
    if (!this.inbound[lane].hasPartialMessage) return;
    this.stalls[lane] = setTimeout(() => {
      this.stalls[lane] = null;
      try {
        if (this.inbound[lane].expire()) this.onStall(lane);
      } catch {
        this.onStall(lane);
      }
    }, ATTACHMENT_TRANSFER_PACKET_STALL_MS);
  }
}

class AttachmentPeerPacketQuota implements AttachmentTransferPacketQuota {
  private readonly retained: Record<AttachmentTransferPacketDirection, number> = {
    incoming: 0,
    outgoing: 0,
  };

  constructor(private readonly maximumBytes: number) {}

  reserve(direction: AttachmentTransferPacketDirection, bytes: number): boolean {
    if (!Number.isSafeInteger(bytes) || bytes < 0) return false;
    const next = this.retained[direction] + bytes;
    if (!Number.isSafeInteger(next) || next > this.maximumBytes) return false;
    this.retained[direction] = next;
    return true;
  }

  release(direction: AttachmentTransferPacketDirection, bytes: number): void {
    this.retained[direction] = Math.max(0, this.retained[direction] - bytes);
  }
}
