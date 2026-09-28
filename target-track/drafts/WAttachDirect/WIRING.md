# WAttachDirect — phase-2 handoff (self-sufficient)

Slice: W-ATTACH direct + peer half of Stage 2W. Ports v2 (`apps/worker/src/…` in this worktree):
`attachments/attachment-direct-{session,socket,frames}.ts`, `attachments/attachment-peer-{connection,owner,packet-budget,packet-port,request-validation}.ts`,
`attachments/local-ui-attachment-socket.ts`, `attachments/attachment-transfer-port.ts` (agreed: mine, not WAttach's),
the attachment half of `boot/boot-local-terminal.ts` + `transport/coord-link-direct-deps.ts` (onLocalAttachmentPeerOffer/Cancel), the
`localAttachmentPeerOffer`/`localAttachmentPeerCancel` cases of `transport/coord-link-direct-terminal.ts`, and v2 protocol
`packages/protocol/src/attachment-transfer{,-packets,-packet-queue}.ts` into roost-protocol.
Worktree `/home/almalinux/repos/roost-v3-worker`. Drafts root: `target-track/drafts/WAttachDirect/` mirrors repo paths.
All drafts are rustfmt-formatted (`rustfmt --edition 2024`, own files only) and every file is ≤400 lines. NOTHING has been compiled:
expect type/name fix-ups against the landed sibling code. Rules: no unwrap/expect outside tests, `//!` headers kept, tracing per transition.

## 1. Draft files → destination (move verbatim, then compile)

roost-protocol (cross-owner edit, report it):
- `crates/roost-protocol/src/attachment_transfer/mod.rs` — every v2 `ATTACHMENT_TRANSFER_*` constant with the prefix dropped (ms = u64, bytes/counts = usize), `PeerChannelLane{Control,Data}`, `PeerDataChannelDefinition` + `PEER_DATA_CHANNELS` (ids 0/1, labels from `versioning::CHANNEL_ATTACHMENT_{CONTROL,DATA}_V1`, protocol "roost.attachment-transfer.v1"), `PeerErrorReason`, `TransferErrorReason` (both `as_str` = v2 codes), `COMPLETE_REASON`, `is_chunk_sha256`, `LOOPBACK_PATH`, `LOOPBACK_SUBPROTOCOL` (= `versioning::SUBPROTOCOL_LOCAL_ATTACHMENT_TRANSFER_V1`). Re-exports the packet API.
- `…/attachment_transfer/packets.rs` — `AttachmentTransferPacketDirection`, `AttachmentTransferPacketError` (+as_str v2 codes), header/packet types, `AttachmentTransferPacketQuota` trait (`reserve(&mut self, direction, bytes)->bool`, `release`), `encode_/parse_attachment_transfer_packet`, `AttachmentTransferPacketAssembler<Q>` (`push(&[u8], now_ms:u64)`, `expire(now_ms)`, `reset`, `has_partial_message`, `is_closed`).
- `…/attachment_transfer/packet_queue.rs` — `AttachmentTransferPacketQueue<Q>` (`enqueue(Vec<u8>)->Result<bool,_>`, `next_fragment()` → fragment with `bytes()`/`commit(self)`, `clear`, `reset`, `queued_bytes`, `message_count`).
- `crates/roost-protocol/tests/attachment_transfer_packets.rs` — port of `packages/protocol/tests/attachment-transfer-packets.test.ts` (6 cases) + the digest/channel case of `attachment-transfer.test.ts:263-298`. v2's "absent/non-finite/negative clock" case is not portable (`now_ms: u64`) — say so in the report.
NOTE: roost-client-core has its own client-side copy (`crates/roost-client-core/src/client/attachments/packets*.rs`, concrete quota, no worker-wide share). Report it as an open dedupe item for the integrator (client-core should re-export roost_protocol's); do not edit client-core.

roost-worker (`crates/roost-worker/src/attachments/`):
- `transfer_port.rs` — `AttachmentTransferPort` trait (socket_id, kind, is_open, `send(Vec<u8>, PeerChannelLane)->SendResult`, `close(Option<u16>,&str)`, `close_after_drain`, `mark_authenticated`), `SendResult`, `PortKind`. Contract: no method calls back into the handler synchronously.
- `direct_frames.rs` — ready/ack/status/closed server-frame encoders on the control lane (v2 attachment-direct-frames.ts).
- `direct_session.rs` — `AttachmentPortSession` (serial identity, expected_peer, metadata, terminal, lease, setup/lease timers, write_pending, `holds_admitted_slot`), `DirectState` (`fail`, `retire_unacknowledged_route`, `close_after_terminal_frame`, `remove_session`, `count_sessions`), `Refusal`.
- `direct_chunks.rs` — `begin_chunk` (sync refusals, write_pending), `settle_chunk` (progress/commit/failed), `accept_status_request`, acks.
- `direct_sockets.rs` — `AttachmentDirectSockets` (Clone, Arc<Inner>, std Mutex): `new(AttachmentDirectSocketsDeps{grants, operations, worker_fingerprint, worker_epoch})` (subscribes to grant changes via Weak), `open_loopback_port`, `open_peer_port(port, expected)->bool`, `receive_frame(socket_id, DirectLane, &[u8]) -> Option<DirectWrite>` (sync half runs now; the returned future is the write+ack; callers `tokio::spawn` it, tests `.await` it), `close_port`, `revoke_device`, `dispose`. `DirectLane{Loopback, Peer(PeerChannelLane)}`.
- `direct_hello.rs` — `accept_hello` (grant verify OUTSIDE the lock: WAttach's lazy expiry fires a listener inside verify), `handle_grant_change`, `arm_hello_deadline` (loopback only, 3 s → invalid_hello), `arm_lease_timer` (sleeps to lease.deadline(), re-reads, fails grant_unavailable on expiry).
- `direct_loopback.rs` — `LoopbackAttachmentTransferPort` (v2 local-ui-attachment-socket.ts; backpressure cap 4×1 MiB) and `impl door::loopback::LoopbackHandlers for AttachmentDirectSockets` (Port = that type; on_message spawns the write).
- `peer_request_validation.rs` — `valid_attachment_peer_offer_identity` + 2 unit tests.
- `peer_budget.rs` — `AttachmentPeerPacketBudget` (worker-wide 32 MiB data ceiling), per-peer `AttachmentPeerPacketPeerBudget{control(),data(),dispose()}`, `AttachmentPeerLaneQuota` implements the protocol quota trait; 2 unit tests.
- `peer_packet_port.rs` — `AttachmentPeerIngress` trait, `lane_index`, `AttachmentPeerPacketPort` incoming half (reassembly, bounds, hello/stall timers, `close_locked` which posts the close reason on an mpsc to the connection; ingress `on_close` is delivered by the connection task, never synchronously).
- `peer_packet_egress.rs` — flush/close_if_drained + `impl AttachmentTransferPort for AttachmentPeerPacketPort`.
- `peer_connection.rs` — `AttachmentPeerConnection` over WPeer's `crate::peer::native` (`NativePeerFactory::create`, negotiated channels from `PEER_DATA_CHANNELS`, answer with ≥1 candidate within min(3 s, deadline), fingerprint check on `Connected`, event task `drive_connection`), `AttachmentPeerConnectionFailure`, `OpenAttachmentPeerPort`, `ConnectionClosed`.
- `peer_owner.rs` — `AttachmentPeerOwner` (Clone): `new(AttachmentPeerOwnerDeps{process_epoch, enabled, bind_address, port_range, is_current_coordinator, authorize_grant, open_peer_port, native_loader, packet_budget})`, `bootstrap()` (tokio OnceCell), `offer(req, RequestBudget, LinkFence) -> OwnerFuture<Result<WLocalAttachmentPeerAnswer, PeerErrorReason>>` (admission + pending reservation SYNC so a following cancel finds it; negotiation in the future), `cancel`, `revoke_device`, `cancel_pending_for_coordinator`, `dispose`, `established_count`, `capability_state`.
- `peer_negotiation.rs` — `PendingPeer`, `admission_failure` (v2 order), capacity bounds, `negotiate`, `assert_current`, `connection_closed`.
- `direct_owners.rs` — `AttachmentDirect` bundle (Clone): `new(AttachmentDirectDeps{grants, operations, worker_fingerprint, worker_epoch, peer: PeerTransportConfig, native_loader, coordinator_generation})`, `sockets()`, `bootstrap()`, `revoke_sockets(fp)`, `revoke_peers(fp)`; implements `link_ports::AttachmentPeerPort` (offer: `coordinator_generation.use_generation` first → else ConnectionSuperseded; cancel likewise) and WPeer's `peer::direct::DirectCarrier{cancel_pending_for_coordinator, dispose}` (dispose order sockets → peer owner → grants, v2 disposeDirect).
- `crates/roost-worker/src/runtime/downstream/attachment_peer.rs` — `Dispatcher::attachment_peer_offer` (no owners → `attachment_peer_error(.., Disabled)` now; else `run_owner` with RequestBudget from `budget_ms` + fence; Ok→`LocalAttachmentPeerAnswer`, Err(reason)→error with that reason, panic/cancel→`ice_failed`; all fenced) and `attachment_peer_cancel` (owners → cancel, none → `unowned(..)`).

Tests (`crates/roost-worker/tests/`):
- `attachment_direct_support/mod.rs` — FakeLoopbackPort + DirectFixture (WAttach API: `AttachmentGrantStore::new(epoch, clock)`, `AttachmentOperations::new(AttachmentBase::new(root), system_clock())`, `AttachmentBase::session_dir`). Scratch dir under `std::env::temp_dir()`, removed on Drop.
- `attachment_direct_socket.rs` — the 6 cases of v2 `apps/worker/tests/attachments/attachment-direct-socket.test.ts` (names kept) + loopback hello deadline (paused time).
- `attachment_peer_owner.rs` — v2 `apps/worker/tests/attachments/attachment-peer-owner.test.ts` over WPeer's `tests/peer_support/fake_native.rs` + `TerminalPeerOwner`.
- `attachment_peer_port.rs` — packet-port guards over fake_native (fragment reassembly, data lane closed before admission, admitted data lane, hello deadline).
- `attachment_loopback_upload.rs` — ACCEPTANCE: real door (`mod door_support;` = WDoorHttp's `tests/door_support/mod.rs`, `DoorOptions{attachment_owner: Some(LoopbackOwner::attachment(Arc::new(sockets))), ..}`) + tokio-tungstenite client → file lands.
- `attachment_peer_upload.rs` — ACCEPTANCE: real str0m pair (WPeer's `tests/peer_support/offerer.rs` `BrowserOfferer`; adjust to its real API: start/accept_answer/send/next_event; `wait_channels_open` is my guess — replace with the offerer's actual open-wait) → file lands.
- v2 `apps/worker/tests/local-attachment-ui.test.ts` is ported by WDoorHttp in `tests/local_door_sockets.rs` (route/subprotocol/oversize) — do not duplicate.

## 2. Exact edits to existing files (re-read each right before editing; siblings edit concurrently)

1. `crates/roost-protocol/src/lib.rs`: add `pub mod attachment_transfer;` next to the other `pub mod` lines (after `pub mod agent_conversation_reference;`).
2. `crates/roost-worker/src/attachments/mod.rs` (WAttach owns; WAttach promised to add these lines — verify, add if missing after messaging WAttach): `pub mod` for transfer_port, direct_frames, direct_session, direct_chunks, direct_sockets, direct_hello, direct_loopback, direct_owners, peer_request_validation, peer_budget, peer_packet_port, peer_packet_egress, peer_connection, peer_negotiation, peer_owner. Tests need these `pub`.
3. `crates/roost-worker/src/door/mod.rs` (WDoorHttp's; agreed): replace the three consts `LOCAL_ATTACHMENT_PATH`, `LOCAL_ATTACHMENT_SUBPROTOCOL`, `LOCAL_ATTACHMENT_MAX_PAYLOAD_BYTES` with
   `pub use roost_protocol::attachment_transfer::{LOOPBACK_MAX_PAYLOAD_BYTES as LOCAL_ATTACHMENT_MAX_PAYLOAD_BYTES, LOOPBACK_PATH as LOCAL_ATTACHMENT_PATH, LOOPBACK_SUBPROTOCOL as LOCAL_ATTACHMENT_SUBPROTOCOL};` (v2 local-ui-server.ts:60-63).
4. `crates/roost-worker/src/link_ports.rs`: add
   ```rust
   /// Attachment peers. v2 `onLocalAttachmentPeerOffer`, `onLocalAttachmentPeerCancel`.
   pub trait AttachmentPeerPort: Send + Sync + std::fmt::Debug {
       fn offer(&self, request: DLocalAttachmentPeerOffer, budget: RequestBudget, fence: LinkFence)
           -> OwnerFuture<Result<WLocalAttachmentPeerAnswer, PeerErrorReason>>;
       fn cancel(&self, request: &DLocalAttachmentPeerCancel);
   }
   ```
   and field `pub attachment_peers: Arc<dyn AttachmentPeerPort>` on `DownstreamOwners`; update every `DownstreamOwners { .. }` literal (grep src + tests; test fakes get a recording impl, never a silent no-op).
   Consumer: `runtime/downstream/attachment_peer.rs`.
5. `crates/roost-worker/src/runtime/downstream/mod.rs`: add `mod attachment_peer;`; replace arms
   - `CoordWorkerDownstream::LocalAttachmentPeerOffer(request) => { link.reply(replies::attachment_peer_disabled(&request, &self.process_epoch)); }` → `=> self.attachment_peer_offer(request, received, link),`
   - `CoordWorkerDownstream::LocalAttachmentPeerCancel(_) => { unowned(kind, "the attachment peer owner"); }` → `=> self.attachment_peer_cancel(&request),` (bind `request`).
   Report to the lead: kind, old verbatim reply (the two lines above), new owner `AttachmentDirect` via `AttachmentPeerPort`.
6. `crates/roost-worker/src/runtime/downstream/replies.rs`: rename/generalize `attachment_peer_disabled(request, worker_epoch)` → `attachment_peer_error(request: &DLocalAttachmentPeerOffer, worker_epoch: &str, reason: PeerErrorReason)` with `reason: reason.as_str().to_owned()` (v2 sendAttachmentPeerError). WPeer generalizes `terminal_peer_disabled` on other lines; keep `PEER_DISABLED` if still used by it.
7. `crates/roost-worker/src/runtime/owners.rs`: after WAttach's `operations` and `grants` are built:
   ```rust
   let attachment_direct = AttachmentDirect::new(AttachmentDirectDeps {
       grants: Arc::clone(&grants), operations: operations.clone(),
       worker_fingerprint: <worker fp String>, worker_epoch: <process epoch String>,
       peer: <WPeer's PeerTransportConfig clone>, native_loader: <the SAME NativeLoader WPeer's owner gets>,
       coordinator_generation: <WPeer's CoordinatorGeneration clone>,
   });
   ```
   - WPeer's `DirectTerminal` carriers: push `Arc::new(attachment_direct.clone()) as Arc<dyn DirectCarrier>` (detach → cancel pending, v2 coord-link-deps.ts:154-156; TerminalDirectRetire → dispose).
   - `DownstreamOwners { .., attachment_peers: Arc::new(attachment_direct.clone()) as Arc<dyn AttachmentPeerPort> }`.
   - WAttach's `AttachmentLink::new(operations, grants, attachment_direct.clone())` (its revoke arm calls `revoke_sockets` → `grants.revoke_device` → `revoke_peers`, v2 boot-local-terminal.ts:182-186).
   - `loopback_routes()`: `attachment: None` → `attachment: Some(LoopbackOwner::attachment(Arc::new(attachment_direct.sockets())))`.
   - before the link runs: `attachment_direct.bootstrap().await == AttachmentPeerBootstrapState::Ready` → WPeer's `DirectPeerSupport { attachment, .. }` → hello advertises `CAPABILITY_ATTACHMENT_TRANSFER_PEER_WEBRTC_V1` (v2 main.ts:183-184). Update `runtime/capabilities.rs` tests that assert it is NOT advertised (classify vs v2: advertised iff Ready).
   - shutdown: DirectTerminal disposal covers it; if owners.rs has its own shutdown list, dispose `attachment_direct` via DirectCarrier::dispose there too (once).

## 3. Agreed sibling interfaces (verify against landed code)
- WAttach (`attachments::*`): `upload::{AttachmentOperations::new(AttachmentBase, AttachmentClock), accept_direct_chunk(DirectChunk)->DirectChunkOutcome (async, includes v2 syncAttachmentOperationProgress), status(session,upload)->AttachmentOperationStatus, detach_direct_carrier(id)}`, `DirectChunk{upload_id,session_id,filename,short_path,total_bytes:u64,data,last,seq:u32,offset:u64,chunk_sha256,carrier_id}`, `DirectChunkOutcome{Progress(Receipt),Committed{abs_path,receipt},Failed(AttachmentOperationError)}`; `receipts::{AttachmentOperationReceipt{seq,next_seq,bytes_received,chunk_sha256,committed,abs_path}, AttachmentOperationStatus(+to_proto), AttachmentOperationError(+reason()->TransferErrorReason)}`; `grants::{AttachmentGrantStore::{new,system,subscribe(GrantListener)->GrantSubscription(cancel), install, current, verify, authorize_peer(&PeerGrantRequest), revoke_device, dispose}, GrantChange{Installed{grant,previous},Removed{grant,reason}}, GrantRemovalReason, PeerGrantAuthorization{Authorized,GrantUnavailable,Expired}, PeerGrantRequest}` — listeners run with NO store lock held; `transfer_admission::{AttachmentPeerExpectedTuple, AttachmentUploadMetadata, admit_attachment_transfer_hello(hello, &store, Option<&tuple>)->Result<_, TransferErrorReason>, attachment_metadata_matches_grant}`; `transfer_lease::AttachmentTransferLease::{start(Instant)->Self, allows_activity(&mut,Instant), note_valid_activity(&mut,Instant), deadline()}`; `store_paths::AttachmentBase`; `attachments::{AttachmentClock, system_clock}`. WAttach's drafts: `target-track/drafts/WAttach/`.
- WPeer (`crate::peer`): `native::{NativePeer, NativePeerFactory, NativeLoader, NativePeerConfig, NativeChannelSpec{id:u16,label,ordered,protocol}, NativePeerEvent, NativePeerEvents, remote_fingerprint_matches, str0m_loader}`, `PeerTransportConfig{enabled,bind_address,port_range}`, `coordinator_generation::CoordinatorGeneration{use_generation,is_current,clear}`, `direct::{DirectCarrier, DirectPeerSupport}`, `TerminalPeerOwner`/`TerminalPeerOwnerDeps`/`TerminalPeerPacketIngress`/`TerminalPeerPacketPort`/`ExpectedPeer` (only for the ported mixed test). Test support: `tests/peer_support/fake_native.rs` (`FakeNative::new/loader/peers`, `FakePeer::emit/open_channel/is_closed/sent`, `OFFER_SDP`, `OFFER_FINGERPRINT`), `tests/peer_support/offerer.rs` (`BrowserOfferer`). WPeer's drafts: `target-track/drafts/WPeer/`.
- WDoorHttp (`crate::door`): `loopback::{LoopbackHandlers (port/on_open/on_message/on_close -> anyhow::Result<()>), LoopbackOwner::{terminal,attachment}, LoopbackRoutes}`, `door::{LoopbackSocket (send(Vec<u8>)->LoopbackSend, close(u16,&str), is_open), LoopbackSend{Written,Queued,Dropped}}`. The pump mints socket ids, logs open/close, drops text frames, enforces the 1 MiB attachment cap, calls on_close exactly once, and send/close never call handlers.

## 4. Decisions made (parity notes for the report)
- Sync-then-async split everywhere v2 relied on "runs synchronously until the first await": `receive_frame` sets write_pending before returning; `AttachmentPeerOwner::offer` reserves the pending entry before returning (a cancel frame right after an offer finds it).
- Peer-port close → ingress `on_close` is delivered from the connection task (v2 did it synchronously inside `close`); needed because `direct_sockets` calls port methods under its own lock. Observable order preserved (sockets session removed, then connection closed).
- Capacity-refused peer port: `open_peer_port` → None → connection error `connection_superseded` (same code v2 reaches via the attachIngress throw).
- `now_ms` for packet reassembly: per-port `tokio::time::Instant` origin (paused-time testable).
- Lease timer uses std `Instant` (WAttach's lease type); hello/stall/drain/ack timers use tokio sleeps.

## 5. Tests to run (phase 2)
`/tmp/wcargo.sh test -p roost-protocol --test attachment_transfer_packets`; `/tmp/wcargo.sh test -p roost-worker --test attachment_direct_socket --test attachment_peer_owner --test attachment_peer_port --test attachment_loopback_upload --test attachment_peer_upload`; `/tmp/wcargo.sh test -p roost-worker --lib attachments::`; clippy `-p roost-worker -p roost-protocol --all-targets -- -D warnings`. Compile via `/tmp/wcheck.sh 'attachment|attachments/|downstream/attachment_peer'`.

## 6. Planned mutations (each: mutate, run, see the named test fail, revert, record file:line)
1. `direct_hello.rs` replay fence: make `replayed` always false → `a_replayed_grant_cannot_admit_a_second_carrier_or_disturb_the_admitted_upload` fails.
2. `direct_sockets.rs` `open_loopback_port`: count ALL sessions instead of `!holds_admitted_slot()` → `an_admitted_upload_frees_its_pre_hello_slot_for_the_next_loopback_socket` fails.
3. `direct_hello.rs` `handle_grant_change`: drop the `Removed{Expired}` early return → no v2 test catches it directly (expiry is lazy in WAttach's store); instead mutate `revoke_device` filter (`!=`) → `explicit_device_revocation_closes_a_leased_port_after_grant_expiry` fails.
4. `direct_chunks.rs` `settle_chunk` Committed: skip `send_attachment_closed(COMPLETE)` → `writes_exact_bytes_then_acks_each_chunk_and_returns_the_committed_path` fails.
5. `direct_hello.rs` `arm_hello_deadline`: sleep `HELLO_DEADLINE_MS * 2` → `a_loopback_socket_that_never_says_hello_is_refused_at_the_deadline` fails.
6. `peer_packet_port.rs` `reassemble`: remove the `!authenticated && lane != Control` guard → `the_data_channel_is_closed_to_a_peer_that_was_never_admitted` fails.
7. `peer_packet_port.rs` `channel_opened`: never arm the hello timer → `an_unadmitted_peer_is_retired_at_the_hello_deadline` fails.
8. `peer_negotiation.rs` `connection_closed`: skip `state.active.remove` → `attachment_peer_failure_leaves_the_terminal_peer_established` fails.
9. `peer_budget.rs` `reserve`: skip the worker-wide `reserve_worker_data` → `the_worker_wide_data_ceiling_refuses_a_peer_that_its_own_lane_would_admit` fails.
10. protocol `packets.rs` `accept`: drop the `offset != partial.bytes.len()` check → `rejects_terminal_magic_wrong_versions_malformed_order_and_frames_above_one_mib` fails.
11. `peer_request_validation.rs` `is_uuid` index 14 range → accept any hex → `refuses_an_unversioned_peer_…` fails.

## 7. Consumers (for the report)
`SendResult`/`PortKind` → `direct_*`/`direct_frames`; `DirectLane` → `direct_loopback`, `direct_owners::DirectPeerIngress`; `PeerErrorReason` → `runtime/downstream/attachment_peer.rs` via replies; `TransferErrorReason` → `direct_*` failure acks + WAttach's receipts; `AttachmentPeerBootstrapState::Ready` → owners.rs capability; `AttachmentPeerPort` → Dispatcher arms; `DirectCarrier` impl → WPeer's DirectTerminal detach/retire; `revoke_sockets`/`revoke_peers` → WAttach's grant-revoke arm; `LoopbackHandlers` impl → door attachment route.

## 8. Open questions / gaps
- Whether `DownstreamOwners` construction sites in tests (`tests/link_downstream_support/`) need a fake `AttachmentPeerPort`; give them a recording fake, not a no-op.
- `BrowserOfferer` open-channel wait API name (my test calls `wait_channels_open(2)` — adapt).
- Duplicate attachment packet framing in roost-client-core (see §1 note) — integrator decision.
- Nothing in this slice was compiled or run; all mutations are still to be executed.
