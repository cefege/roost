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

Mutations (queued batch /tmp/wad-mutate.py; results in /tmp/wad-mutate.log): see the WAttachDirect
slice report (direct_hello replay fence and hello deadline, direct_sockets
pre-hello slot count and device revoke filter, direct_chunks complete close,
peer_packet_port unauthenticated data lane and hello timer, peer_negotiation
active removal, peer_budget worker-wide ceiling, protocol packets offset check,
peer_request_validation uuid version, the attachment_peer arm refusal reason).

Cross-owner edits: roost-protocol (new attachment_transfer module + lib.rs mod
line); door/mod.rs LOCAL_ATTACHMENT_* now re-export the protocol constants
(same names and values, agreed with the door owner).
