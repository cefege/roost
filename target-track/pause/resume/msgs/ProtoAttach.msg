protocol: attachment transfer contract — loopback route, bounds, peer packet framing and send queue

Ports v2 packages/protocol/src/attachment-transfer.ts (+ its peer packet framing and send
queue) into roost-protocol attachment_transfer/{mod,packets,packet_queue}.rs, so the
worker's direct/peer attachment carriers and the browser read one set of bounds,
deadlines, channel names and refusal vocabularies.

Self-contained (roost-protocol: lib.rs mod line, attachment_transfer/*, tests
attachment_transfer_packets.rs). Cherry-pick target: coord and web tracks as they need it.

Production consumers:
- AttachmentTransferPacketAssembler/parse/encode, AttachmentTransferPacketQueue <-
  roost-worker attachments/peer_packet_port.rs, peer_budget.rs.
- loopback route / chunk bounds / deadlines <- roost-worker attachments/direct_*.rs and door.

Mutations (mutate -> watch fail -> revert):
- packets.rs offset check `|| header.offset_bytes as usize != partial.bytes.len()` -> `|| false`
  -> attachment_transfer_packets::rejects_terminal_magic* (result recorded in the
  WAttachDirect commit's batch log).
