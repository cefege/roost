<!-- Attachment-transfer contract: exact upload grants, loopback/WebRTC framing, ACK recovery, and RPC relay. -->
<!-- One upload descriptor is immutable; workers ACK only after the destination write. -->
<!-- Browser carrier order is loopback, then WebRTC, then AttachFileChunk relay. -->

# Attachments

## Purpose

Attachment upload uses one immutable descriptor (`session_id`, `upload_id`, filename/path mode, and `total_bytes`) across three carriers. The browser prefers worker loopback, then attachment-specific WebRTC, then the coordinator `AttachFileChunk` relay. Every carrier reports bytes received and can resolve a lost ACK without blindly retransmitting accepted bytes.

## Messages

| Message | File | Meaning |
| --- | --- | --- |
| `AttachmentsGrantDirectRequest/Response`, `SessionsNegotiateAttachmentPeerRequest/Response` | `protocol/proto/roost/v1/coordinator.proto:256-308` | Short-lived exact-descriptor grant and bounded attachment-peer offer/answer. |
| `AttachFileChunkRequest/Response` | `protocol/proto/roost/v1/coordinator.proto` | Coordinator relay fallback for one in-order chunk. |
| `AttachmentTransferHello`, `Ready`, `Chunk`, `Ack`, `Closed`, `StatusRequest`, `Status` | `protocol/proto/roost/v1/attachment_transfer.proto:8-83` | Direct carrier descriptor, ordered data, durable receipt, and close. |
| `roost-attachment-control-v1`, `roost-attachment-data-v1` | `packages/protocol/src/attachment-transfer.ts:58-73` | Ordered WebRTC lanes; protocol `roost.attachment-transfer.v1`. |
| `/ws/local-attachment-transfer`; `roost-local-attachment-transfer-v1` | `packages/protocol/src/attachment-transfer.ts:8-9` | Worker loopback path and subprotocol. |

## State machine

1. Browser creates an upload identity and descriptor, then calls `AttachmentsGrantDirect` naming exact session, worker, tab, upload, filename/path mode, and total bytes. Coordinator authorizes the browser/session/tab and installs the grant on the exact worker before returning its secret and worker epoch.
2. If the browser reaches that worker's loopback door, it opens `/ws/local-attachment-transfer`, sends `AttachmentTransferHello`, and waits for `AttachmentTransferReady`. The worker recomputes/verifies the grant secret digest and requires the hello descriptor to match the installed immutable grant exactly.
3. Otherwise the client requests attachment WebRTC negotiation. Coordinator signaling remains bound to the current device/tab, grant, worker connection/epoch, and peer ID. After `Ready`, browser and worker use separate ordered control/data channels under `roost.attachment-transfer.v1`; this framing is independent from terminal-peer framing.
4. Browser sends contiguous `AttachmentTransferChunk` values with `seq`, `offset`, `last`, and lowercase SHA-256. Worker writes the chunk synchronously to the destination, advances the durable receipt, then ACKs `bytes_received`; the final successful ACK includes `abs_path`.
5. A lost or ambiguous direct ACK is resolved with `AttachmentTransferStatusRequest`. `next_seq`, `bytes_received`, and last digest determine whether to continue or treat the upload committed; accepted bytes are not replayed on another carrier.
6. If neither direct carrier is available, the browser sends the same ordered upload through coordinator `AttachFileChunk` relay to the owning worker. Carrier choice is per upload; terminal direct state is not reused.
7. Closing releases only the exact upload/peer/loopback carrier. Session close and exact worker retirement prevent new grants; durable receipts, not connection closure, determine whether bytes committed.

## Limits

| Constant | Value | TypeScript source |
| --- | ---: | --- |
| `ATTACHMENT_TRANSFER_DIRECT_CHUNK_BYTES` | `512 KiB` | `packages/protocol/src/attachment-transfer.ts:10` |
| `ATTACHMENT_TRANSFER_CHUNK_SHA256_HEX_LENGTH` | `64` | `packages/protocol/src/attachment-transfer.ts:11` |
| `ATTACHMENT_TRANSFER_LOOPBACK_MAX_PAYLOAD_BYTES` | `1 MiB` | `packages/protocol/src/attachment-transfer.ts:12` |
| `ATTACHMENT_TRANSFER_PACKET_MAX_BYTES` / payload | `16,384 / 16,364 bytes` | `packages/protocol/src/attachment-transfer.ts:16-18` |
| `ATTACHMENT_TRANSFER_PACKET_LOGICAL_FRAME_MAX_BYTES` | `1 MiB` | `packages/protocol/src/attachment-transfer.ts:19` |
| `ATTACHMENT_TRANSFER_PACKET_STALL_MS` | `10,000 ms` | `packages/protocol/src/attachment-transfer.ts:20` |
| `ATTACHMENT_TRANSFER_MAX_CHUNKS_IN_FLIGHT` | `1` | `packages/protocol/src/attachment-transfer.ts:21` |
| `ATTACHMENT_TRANSFER_MAX_ACTIVE_PER_WORKER` / browser document | `8 / 8` | `packages/protocol/src/attachment-transfer.ts:22-23` |
| `ATTACHMENT_TRANSFER_ACTIVE_MAX_MS` / `IDLE_MS` | `12 h / 5 min` | `packages/protocol/src/attachment-transfer.ts:24-25` |
| `ATTACHMENT_TRANSFER_GRANT_TTL_MS` / ACK deadline | `60,000 / 15,000 ms` | `packages/protocol/src/attachment-transfer.ts:26` |
| `ATTACHMENT_TRANSFER_GRANT_ACK_DEADLINE_MS` | `8,000 ms` | `packages/protocol/src/attachment-transfer.ts:27` |
| `ATTACHMENT_TRANSFER_PEER_MAX_NEGOTIATIONS_PER_WORKER` | `4` | `packages/protocol/src/attachment-transfer.ts:32` |
| `ATTACHMENT_TRANSFER_PEER_MAX_PENDING_NEGOTIATIONS` / device | `64 / 8` | `packages/protocol/src/attachment-transfer.ts:33-34` |
| `ATTACHMENT_TRANSFER_PEER_NEGOTIATION_DEADLINE_MS` | `15,000 ms` | `packages/protocol/src/attachment-transfer.ts:35` |
| `ATTACHMENT_TRANSFER_PEER_CONTROL_QUEUE_MAX_BYTES` / data queue | `128 KiB / 1 MiB` | `packages/protocol/src/attachment-transfer.ts:42-43` |
| `ATTACHMENT_TRANSFER_PEER_WORKER_DATA_QUEUE_MAX_BYTES` | `32 MiB` | `packages/protocol/src/attachment-transfer.ts:44` |

## Errors

- Attachment transfer close/error reasons: `invalid_hello`, `grant_unavailable`, `upload_not_found`, `upload_mismatch`, `chunk_out_of_order`, `chunk_offset_mismatch`, `chunk_sha256_mismatch`, `chunk_too_large`, `total_bytes_mismatch`, `write_failed`; success reason is `complete` (`packages/protocol/src/attachment-transfer.ts:88-102`).
- Attachment peer error reasons: `disabled`, `native_unavailable`, `invalid_offer`, `grant_unavailable`, `capacity`, `expired`, `connection_superseded`, `ice_failed` (`packages/protocol/src/attachment-transfer.ts:75-86`).
- `AttachmentTransferPacketErrorCode`: `packet-size`, `packet-magic`, `packet-version`, `packet-header`, `message-id`, `message-id-wrap`, `message-size`, `fragment-order`, `fragment-stalled`, `quota`, `allocation`, `clock`, `closed` (`packages/protocol/src/attachment-transfer-packets.ts:27-40`).
- A hello descriptor mismatch is `upload_mismatch`; a missing durable upload is `upload_not_found`. Status, not error inference from socket close, resolves ambiguous ACKs.

## Reference implementation

- Shared browser-safe contract: `packages/protocol/src/attachment-transfer.ts`, `packages/protocol/src/attachment-transfer-packets.ts`
- Proto frames: `protocol/proto/roost/v1/attachment_transfer.proto`
- Worker loopback/direct owner: `apps/worker/src/local-ui-server.ts`, `apps/worker/src/attachment-direct-socket.ts`, `apps/worker/src/attachment-peer-owner.ts`
- Coordinator grant/relay/signaling: `apps/coord/src/attachments/`
- Browser selection/send/ACK recovery: `apps/web/src/client/attachments/attachmentDirect.ts`, `apps/web/src/client/carriers/attachment-loopback.ts`, `apps/web/src/client/attachments/attachmentPeer.ts`, `apps/web/src/client/attachments/attachmentTransfer.ts`
