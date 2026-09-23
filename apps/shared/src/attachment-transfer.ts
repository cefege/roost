// Browser-safe attachment direct-carrier protocol constants.
// Web, coordinator, and worker import this contract for the same bounded
// loopback and WebRTC setup without sharing terminal transport state.
// This module deliberately imports no Node runtime APIs.

export const ATTACHMENT_TRANSFER_PEER_WEBRTC_CAPABILITY = "attachment-transfer-peer-webrtc-v1";

export const ATTACHMENT_TRANSFER_LOOPBACK_PATH = "/ws/local-attachment-transfer";
export const ATTACHMENT_TRANSFER_LOOPBACK_SUBPROTOCOL = "roost-local-attachment-transfer-v1";
export const ATTACHMENT_TRANSFER_DIRECT_CHUNK_BYTES = 512 * 1024;
export const ATTACHMENT_TRANSFER_CHUNK_SHA256_HEX_LENGTH = 64;
export const ATTACHMENT_TRANSFER_LOOPBACK_MAX_PAYLOAD_BYTES = 1024 * 1024;
export const ATTACHMENT_TRANSFER_PACKET_MAGIC = 0x3150_5441;
export const ATTACHMENT_TRANSFER_PACKET_VERSION = 1;
export const ATTACHMENT_TRANSFER_PACKET_HEADER_BYTES = 20;
export const ATTACHMENT_TRANSFER_PACKET_MAX_BYTES = 16_384;
export const ATTACHMENT_TRANSFER_PACKET_MAX_PAYLOAD_BYTES = ATTACHMENT_TRANSFER_PACKET_MAX_BYTES
  - ATTACHMENT_TRANSFER_PACKET_HEADER_BYTES;
export const ATTACHMENT_TRANSFER_PACKET_LOGICAL_FRAME_MAX_BYTES = 1024 * 1024;
export const ATTACHMENT_TRANSFER_PACKET_STALL_MS = 10_000;
export const ATTACHMENT_TRANSFER_MAX_CHUNKS_IN_FLIGHT = 1;
export const ATTACHMENT_TRANSFER_MAX_ACTIVE_PER_WORKER = 8;
export const ATTACHMENT_TRANSFER_MAX_ACTIVE_PER_BROWSER_DOCUMENT = 8;
export const ATTACHMENT_TRANSFER_ACTIVE_MAX_MS = 12 * 60 * 60_000;
export const ATTACHMENT_TRANSFER_IDLE_MS = 5 * 60_000;
export const ATTACHMENT_TRANSFER_GRANT_TTL_MS = 60_000;
export const ATTACHMENT_TRANSFER_GRANT_ACK_DEADLINE_MS = 8_000;
export const ATTACHMENT_TRANSFER_MAX_PENDING_GRANTS = 64;
export const ATTACHMENT_TRANSFER_MAX_PENDING_GRANTS_PER_DEVICE = 8;
export const ATTACHMENT_TRANSFER_MAX_PENDING_STATUS_REQUESTS = 64;

export const ATTACHMENT_TRANSFER_PEER_MAX_NEGOTIATIONS_PER_WORKER = 4;
export const ATTACHMENT_TRANSFER_PEER_MAX_PENDING_NEGOTIATIONS = 64;
export const ATTACHMENT_TRANSFER_PEER_MAX_PENDING_NEGOTIATIONS_PER_DEVICE = 8;
export const ATTACHMENT_TRANSFER_PEER_NEGOTIATION_DEADLINE_MS = 15_000;
export const ATTACHMENT_TRANSFER_PEER_ICE_GATHERING_DEADLINE_MS = 3_000;
export const ATTACHMENT_TRANSFER_PEER_NATIVE_ANSWER_DEADLINE_MS = 8_000;
export const ATTACHMENT_TRANSFER_HELLO_DEADLINE_MS = 3_000;
export const ATTACHMENT_TRANSFER_ACK_DEADLINE_MS = 15_000;
export const ATTACHMENT_TRANSFER_STATUS_DEADLINE_MS = 8_000;

export const ATTACHMENT_TRANSFER_PEER_CONTROL_QUEUE_MAX_BYTES = 128 * 1024;
export const ATTACHMENT_TRANSFER_PEER_DATA_QUEUE_MAX_BYTES = 1024 * 1024;
export const ATTACHMENT_TRANSFER_PEER_WORKER_DATA_QUEUE_MAX_BYTES = 32 * 1024 * 1024;

export type AttachmentTransferPeerChannelLane = "control" | "data";

export const ATTACHMENT_TRANSFER_PEER_DATA_CHANNEL_PROTOCOL = "roost.attachment-transfer.v1";

export interface AttachmentTransferPeerDataChannelDefinition {
  readonly lane: AttachmentTransferPeerChannelLane;
  readonly id: 0 | 1;
  readonly label: string;
  readonly ordered: true;
  readonly protocol: typeof ATTACHMENT_TRANSFER_PEER_DATA_CHANNEL_PROTOCOL;
}

export const ATTACHMENT_TRANSFER_PEER_DATA_CHANNELS: readonly AttachmentTransferPeerDataChannelDefinition[] = [
  {
    lane: "control",
    id: 0,
    label: "roost-attachment-control-v1",
    ordered: true,
    protocol: ATTACHMENT_TRANSFER_PEER_DATA_CHANNEL_PROTOCOL,
  },
  {
    lane: "data",
    id: 1,
    label: "roost-attachment-data-v1",
    ordered: true,
    protocol: ATTACHMENT_TRANSFER_PEER_DATA_CHANNEL_PROTOCOL,
  },
];

export const ATTACHMENT_TRANSFER_PEER_ERROR_REASONS = [
  "disabled",
  "native_unavailable",
  "invalid_offer",
  "grant_unavailable",
  "capacity",
  "expired",
  "connection_superseded",
  "ice_failed",
] as const;

export type AttachmentTransferPeerErrorReason = (typeof ATTACHMENT_TRANSFER_PEER_ERROR_REASONS)[number];

export const ATTACHMENT_TRANSFER_ERROR_REASONS = [
  "invalid_hello",
  "grant_unavailable",
  "upload_not_found",
  "upload_mismatch",
  "chunk_out_of_order",
  "chunk_offset_mismatch",
  "chunk_sha256_mismatch",
  "chunk_too_large",
  "total_bytes_mismatch",
  "write_failed",
] as const;

export type AttachmentTransferErrorReason = (typeof ATTACHMENT_TRANSFER_ERROR_REASONS)[number];
export const ATTACHMENT_TRANSFER_COMPLETE_REASON = "complete";

/** Accepts the lowercase hexadecimal SHA-256 digest echoed on direct chunks. */
export function isAttachmentTransferChunkSha256(value: string): boolean {
  return value.length === ATTACHMENT_TRANSFER_CHUNK_SHA256_HEX_LENGTH
    && /^[0-9a-f]+$/u.test(value);
}
