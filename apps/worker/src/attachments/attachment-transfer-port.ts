// Common direct-attachment port contract for loopback and WebRTC carriers.
// AttachmentDirectSockets owns protobuf admission and destination writes;
// carriers own bytes, backpressure, and their independent close lifecycle.

import type { AttachmentTransferPeerChannelLane } from "@roost/protocol/attachment-transfer";

export type AttachmentTransferSendResult = "accepted" | "backpressured" | "refused";

export interface AttachmentTransferPort {
  readonly socketId: string;
  readonly kind: "loopback" | "webrtc";
  readonly open: boolean;
  send(bytes: Uint8Array, lane: AttachmentTransferPeerChannelLane): AttachmentTransferSendResult;
  close(code?: number, reason?: string): void;
  /** A WebRTC port waits for its ordered control queue; loopback closes immediately. */
  closeAfterDrain(reason: string): void;
  /** WebRTC refuses data-channel chunks until AttachmentTransferHello is admitted. */
  markAuthenticated(): void;
}
