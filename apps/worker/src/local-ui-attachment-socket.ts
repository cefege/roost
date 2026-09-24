// Loopback attachment port adapter for the worker's local UI WebSocket.
// It keeps attachment bytes on the separate subprotocol while direct socket
// admission owns protobuf authentication, acknowledgements, and close semantics.

import { ATTACHMENT_TRANSFER_LOOPBACK_MAX_PAYLOAD_BYTES } from "@roost/protocol/attachment-transfer";
import type {
  AttachmentTransferPort,
  AttachmentTransferSendResult,
} from "./attachment-transfer-port.ts";
import type { BunLoopbackSocket } from "./local-ui-terminal-socket.ts";

const MAX_BACKPRESSURE_BYTES = ATTACHMENT_TRANSFER_LOOPBACK_MAX_PAYLOAD_BYTES * 4;

export interface LocalAttachmentSocket extends AttachmentTransferPort {
  readonly kind: "loopback";
}

export interface LocalAttachmentSocketHandlers {
  onOpen(port: AttachmentTransferPort): void;
  onMessage(port: AttachmentTransferPort, data: Uint8Array): void;
  onClose(port: AttachmentTransferPort): void;
}

export class LoopbackAttachmentTransferPort implements LocalAttachmentSocket {
  readonly kind = "loopback" as const;
  private backpressuredBytes = 0;

  constructor(
    readonly socketId: string,
    private readonly socket: BunLoopbackSocket,
  ) {}

  get open(): boolean {
    return this.socket.open;
  }

  send(bytes: Uint8Array, _lane: "control" | "data"): AttachmentTransferSendResult {
    if (!this.socket.open) return "refused";
    const result = this.socket.send(bytes);
    if (result > 0) {
      this.backpressuredBytes = 0;
      return "accepted";
    }
    if (result < 0) {
      this.backpressuredBytes += bytes.byteLength;
      return this.backpressuredBytes <= MAX_BACKPRESSURE_BYTES ? "backpressured" : "refused";
    }
    return "refused";
  }

  closeAfterDrain(reason: string): void {
    this.close(1000, reason);
  }

  markAuthenticated(): void {}

  close(code?: number, reason?: string): void {
    this.socket.close(code, reason);
  }
}
