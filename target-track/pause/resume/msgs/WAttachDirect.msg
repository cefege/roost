worker: attachment direct and peer carriers

Ports v2 apps/worker/src/attachments/attachment-direct-{session,socket,frames}.ts,
attachment-peer-{connection,owner,packet-budget,packet-port,request-validation}.ts,
attachment-transfer-port.ts and local-ui-attachment-socket.ts, the attachment
half of boot/boot-local-terminal.ts and transport/coord-link-direct-deps.ts, and
the localAttachmentPeerOffer/Cancel cases of coord-link-direct-terminal.ts. Adds
roost_protocol::attachment_transfer (packages/protocol/src/attachment-transfer{,
-packets,-packet-queue}.ts) so the worker and the browser read one set of
bounds, lanes and refusal codes.

Consumers: AttachmentPeerPort -> runtime/downstream/attachment_peer.rs (the
LocalAttachmentPeerOffer arm, formerly `link.reply(replies::attachment_peer_disabled(..))`
on every offer, and the LocalAttachmentPeerCancel arm, formerly
`unowned(kind, "W-PEER")`, now route to attachments::direct_owners::AttachmentDirect);
AttachmentDirect is registered as peer::DirectTerminal's DirectCarrier (link
detach cancels pending peers, direct retire/shutdown disposes sockets, peers,
grants); the door's attachment LoopbackOwner is AttachmentDirectSockets
(loopback_routes, formerly None -> v2 404); DirectPeerSupport.attachment is
advertised iff the attachment peer bootstrap is Ready (v2 main.ts:183-184).
PeerErrorReason is what replies::attachment_peer_error (formerly
attachment_peer_disabled) carries.

Mutation batch at resume (WorkerLead; mutbatch.py: co-applied mutants never share a file
or a test binary; KILLED = the named guard failed under the mutant; every file restored):
- M0 roost-worker attachments/direct_hello.rs: `if replayed {` -> `if false && replayed {` => FAILS a_replayed_grant_cannot_admit_a_second_carrier_or_disturb_the_admitted_upload
- M5 roost-worker attachments/peer_packet_port.rs: `if !state.authenticated && lane != PeerChannelLane::Control {` -> `if false {` => FAILS the_data_channel_is_closed_to_a_peer_that_was_never_admitted
- M7 roost-worker attachments/peer_negotiation.rs: `state.active.remove(peer_id);` -> `let _ = peer_id;` => FAILS attachment_peer_failure_leaves_the_terminal_peer_established
- M8 roost-worker attachments/peer_budget.rs: `&& !shared.reserve_worker_data(bytes)` -> `&& !shared.reserve_worker_data(0)` => FAILS attachments::peer_budget::tests::the_worker_wide_data_ceiling_refuses_a_peer_that_its_own_lane_would_admit
- M9 roost-protocol attachment_transfer/packets.rs: `|| header.offset_bytes as usize != partial.bytes.len()` -> `|| false` => FAILS rejects_terminal_magic_wrong_versions_malformed_order_and_frames_above_one_mib
- M11 roost-worker runtime/downstream/attachment_peer.rs: `attachment_peer_error(&refused, &epoch, reason)` -> `attachment_peer_error(&refused, &epoch, PeerErrorReason::IceFailed)` => FAILS attachment_controls_emit_a_typed_answer_and_error_and_route_the_cancel
- M1 roost-worker attachments/direct_sockets.rs: `|session| !session.holds_admitted_slot()` -> `|_session| true` => FAILS an_admitted_upload_frees_its_pre_hello_slot_for_the_next_loopback_socket
- M6 roost-worker attachments/peer_packet_port.rs: `&& state.hello_timer.is_none()` -> `&& false` => FAILS an_unadmitted_peer_is_retired_at_the_hello_deadline
- M10 roost-worker attachments/peer_request_validation.rs: `(b'1'..=b'5').contains(byte)` -> `byte.is_ascii_hexdigit()` => FAILS attachments::peer_request_validation::tests::refuses_an_unversioned_peer_an_empty_or_oversized_id_and_control_characters
- M2 roost-worker attachments/direct_sockets.rs: `metadata.device_fingerprint == device_fingerprint` -> `metadata.device_fingerprint != device_fingerprint` => FAILS explicit_device_revocation_closes_a_leased_port_after_grant_expiry
- M3 roost-worker attachments/direct_chunks.rs: `send_attachment_closed(port.as_ref(), COMPLETE_REASON);` -> `let _ = &port;` => FAILS writes_exact_bytes_then_acks_each_chunk_and_returns_the_committed_path
- M4 roost-worker attachments/direct_hello.rs: `Duration::from_millis(HELLO_DEADLINE_MS)` -> `Duration::from_millis(HELLO_DEADLINE_MS * 2)` => FAILS a_loopback_socket_that_never_says_hello_is_refused_at_the_deadline

Cross-owner edits: roost-protocol (new attachment_transfer module + lib.rs mod
line); door/mod.rs LOCAL_ATTACHMENT_* now re-export the protocol constants
(same names and values, agreed with the door owner).

Series: one of the wave 2+3 per-slice commits (ProtoDoor..WAgentsPrompt). A file shared by
several slices lands whole in the FIRST slice commit that lists it, so intermediate commits
may not build; the track gate (tests x2, clippy, lint, fmt) ran on the series tip.
