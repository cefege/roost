// Separate retained-byte quotas for attachment WebRTC packet queues and assembly.
// Terminal peers never share these counters, so attachment pressure cannot retire
// or delay a terminal carrier.

import {
  ATTACHMENT_TRANSFER_PEER_CONTROL_QUEUE_MAX_BYTES,
  ATTACHMENT_TRANSFER_PEER_DATA_QUEUE_MAX_BYTES,
  ATTACHMENT_TRANSFER_PEER_WORKER_DATA_QUEUE_MAX_BYTES,
} from "@roost/protocol/attachment-transfer";
import type {
  AttachmentTransferPacketDirection,
  AttachmentTransferPacketQuota,
} from "@roost/protocol/attachment-transfer-packets";

export interface AttachmentPeerPacketPeerBudget {
  readonly control: AttachmentTransferPacketQuota;
  readonly data: AttachmentTransferPacketQuota;
  dispose(): void;
}

/** Process-owned attachment packet budget. */
export class AttachmentPeerPacketBudget {
  private retainedDataBytes = 0;
  private disposed = false;

  createPeerBudget(): AttachmentPeerPacketPeerBudget {
    const control = new AttachmentPeerLaneQuota(
      ATTACHMENT_TRANSFER_PEER_CONTROL_QUEUE_MAX_BYTES,
      () => true,
      () => undefined,
    );
    const data = new AttachmentPeerLaneQuota(
      ATTACHMENT_TRANSFER_PEER_DATA_QUEUE_MAX_BYTES,
      (bytes) => this.reserveWorkerData(bytes),
      (bytes) => this.releaseWorkerData(bytes),
    );
    let closed = false;
    return {
      control,
      data,
      dispose: () => {
        if (closed) return;
        closed = true;
        control.dispose();
        data.dispose();
      },
    };
  }

  dispose(): void {
    this.disposed = true;
    this.retainedDataBytes = 0;
  }

  private reserveWorkerData(bytes: number): boolean {
    if (this.disposed || !validByteCount(bytes)) return false;
    if (bytes > ATTACHMENT_TRANSFER_PEER_WORKER_DATA_QUEUE_MAX_BYTES - this.retainedDataBytes) {
      return false;
    }
    this.retainedDataBytes += bytes;
    return true;
  }

  private releaseWorkerData(bytes: number): void {
    if (!validByteCount(bytes)) return;
    this.retainedDataBytes = Math.max(0, this.retainedDataBytes - bytes);
  }
}

class AttachmentPeerLaneQuota implements AttachmentTransferPacketQuota {
  private incomingBytes = 0;
  private outgoingBytes = 0;
  private disposed = false;

  constructor(
    private readonly maxBytes: number,
    private readonly reserveShared: (bytes: number) => boolean,
    private readonly releaseShared: (bytes: number) => void,
  ) {}

  reserve(direction: AttachmentTransferPacketDirection, bytes: number): boolean {
    if (this.disposed || !validByteCount(bytes)) return false;
    const retained = direction === "incoming" ? this.incomingBytes : this.outgoingBytes;
    if (bytes > this.maxBytes - retained || !this.reserveShared(bytes)) return false;
    if (direction === "incoming") this.incomingBytes += bytes;
    else this.outgoingBytes += bytes;
    return true;
  }

  release(direction: AttachmentTransferPacketDirection, bytes: number): void {
    if (!validByteCount(bytes)) return;
    if (direction === "incoming") this.incomingBytes = Math.max(0, this.incomingBytes - bytes);
    else this.outgoingBytes = Math.max(0, this.outgoingBytes - bytes);
    this.releaseShared(bytes);
  }

  dispose(): void {
    if (this.disposed) return;
    this.disposed = true;
    this.releaseShared(this.incomingBytes + this.outgoingBytes);
    this.incomingBytes = 0;
    this.outgoingBytes = 0;
  }
}

function validByteCount(value: number): boolean {
  return Number.isSafeInteger(value) && value >= 0;
}
