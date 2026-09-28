# WPeer — phase-2 wiring handbook (self-sufficient)

Slice W-PEER of Stage 2W (plan `docs/v3-handoff/roost-v3-finish-and-cutover-plan.md` "### Stage 2W", row W-PEER).
Ports v2 `apps/worker/src/terminal/peer/*` (11 files; the test-fault files ONLY as `OfferFault` + real
injection sites) and `apps/worker/src/transport/coord-link-direct-{terminal,deps}.ts` (terminal half) plus the
peer half of `apps/worker/src/boot/boot-local-terminal.ts`. Rules for phase 2 are in the lead's brief (wcargo,
wcheck, ≤400 lines, no fmt, no commit, mutations, done file). Drafts were never compiled: expect small fixes.

## 0. Dependency order
- REQUIRES WDoorTerm's `crate::local_terminal` (drafts at `target-track/drafts/WDoorTerm/crates/roost-worker/src/local_terminal/`)
  to be in the tree: this slice uses `local_terminal::{TerminalPacketPort, PeerTerminalPacketPort,
  HistoryReadReservation, PacketSendResult, ExpectedPeer, PeerIngress, PeerGrantAuthorization,
  GrantRemovalReason, LocalTerminalDoor}` and `link_ports::LocalTerminalGrantPort` (WDoorTerm adds it) — do not
  redefine any of them. If WDoorTerm has not landed, wait for it (`ls target-track/wave-gates/done-WDoorTerm`).
- WAttachDirect (attachment peer) REUSES `crate::peer::native`, `peer::CoordinatorGeneration`,
  `peer::DirectCarrier`, `peer::TerminalPeerPacketIngress`, and `tests/peer_support/{fake_native,offerer}.rs`.

## 1. New files (copy from `target-track/drafts/WPeer/` to the same repo path)
`crates/roost-worker/src/peer/` — `mod.rs` REPLACES the existing 53-line stub (its `OfferFault` moves to `faults.rs`,
still `crate::peer::OfferFault`):
| file | purpose | v2 source |
|---|---|---|
| `mod.rs` | module decls + re-exports | — |
| `config.rs` | `PeerTransportConfig {enabled, bind_address, port_range}` + env parsing | `host/config.ts` parseTerminalPeer* |
| `faults.rs` | `OfferFault` (moved) + `OfferFaultSlot` (arm/consume) | `terminal-peer-test-faults.ts` (offer part) |
| `request_validation.rs` | `valid_terminal_peer_offer_identity` | `terminal-peer-request-validation.ts` |
| `coordinator_generation.rs` | `CoordinatorGeneration` (use/is_current/clear) | `boot-local-terminal.ts` useCoordinatorGeneration |
| `packet_budget.rs` | worker budget, THE TURN (one lock all port ops take = v2's single thread), pressure handlers, deferred effects | `terminal-peer-packet-budget.ts` |
| `peer_budget.rs` | per-peer two-direction budget, quotas, holds | same file, `TerminalPeerPacketPeerBudget` |
| `history_reservation.rs` | pre-read history reservation + queue handoff | `terminal-peer-history-reservation.ts` |
| `packet_port.rs` | `TerminalPeerPacketPort` (impl WDoorTerm's port traits), close/fail, setup deadline, `TerminalPeerPacketIngress` trait (+impl for `PeerIngress`) | `terminal-peer-packet-port.ts` |
| `packet_lanes.rs` | send/flush/receive/stall timers/drain waiters (`impl TerminalPeerPacketPort`) | same file |
| `connection.rs` | `TerminalPeerConnection` (native peer + port, answer, fingerprint check, close) | `terminal-peer-connection.ts` |
| `owner.rs` | `TerminalPeerOwner` (bootstrap, cancel, revoke, detach, dispose), `TerminalPeerOfferFailure`, `PeerBootstrapState` | `terminal-peer-owner.ts` |
| `owner_offer.rs` | offer path: fault sites, admission, capacity, negotiate, promote | same file |
| `direct.rs` | `DirectTerminal` (implements `DirectTerminalPort` + `LocalTerminalGrantPort`), `DirectCarrier`, `DirectPeerSupport`, `DirectLinkLifecycle` | `coord-link-direct-deps.ts`, `boot-local-terminal.ts` |
| `native/mod.rs` | node-datachannel-shaped traits `NativePeer`/`NativePeerFactory`, `NativePeerEvent`, `NativeLoader`, `str0m_loader()`, `remote_fingerprint_matches` | `terminal-peer-native.ts` |
| `native/factory.rs` | `Str0mPeerFactory` (crypto provider, cert per peer, negotiated channels before remote SDP) | same |
| `native/str0m_peer.rs` | str0m `Rtc` engine: drive turn, events→callbacks, libdatachannel-style send buffer (`Ok(false)` = accepted-buffered) + low-water | node-datachannel |
| `native/peer_handle.rs` | `impl NativePeer` (answer = gather → accept_offer → spawn driver) | node-datachannel |
| `native/driver.rs` | tokio task: per-socket readers, str0m timeouts, transmit from matching socket | node-datachannel thread |
| `native/gather.rs` | one UDP socket per local address (bind/port-range), host + STUN srflx candidates | libjuice gathering |
| `native/host_addresses.rs` | Linux `/proc/net/fib_trie` + `/proc/net/if_inet6`, macOS `ifconfig` (no unsafe, no new crate) | libjuice |
| `native/stun.rs` | Binding request + XOR-MAPPED-ADDRESS parse (RFC 5389/5769 vector test) | libjuice |

`crates/roost-worker/src/runtime/downstream/direct.rs` — the 4 direct-terminal arms (`impl Dispatcher`).

Tests (new): `tests/peer_support/fake_native.rs` (v2 owner fixture port; also WAttachDirect's), `tests/peer_support/offerer.rs`
(str0m browser-side offerer), `tests/terminal_peer_owner.rs`, `tests/terminal_peer_packet_port.rs`,
`tests/terminal_peer_str0m.rs`, `tests/link_downstream_direct.rs`.

## 2. Exact edits to existing files (re-read each file first; siblings edit concurrently)
1. `src/link_ports.rs` — append (imports: `roost_proto::{DLocalTerminalPeerCancel, DLocalTerminalPeerOffer, DTerminalDirectRetire, DTerminalTransportProbe, WLocalTerminalPeerAnswer, WTerminalTransportProbeResult}`, `crate::peer::TerminalPeerOfferFailure`):
   ```rust
   /// The direct terminal path (impl: `peer::DirectTerminal`). v2 `onLocalTerminalPeerOffer`,
   /// `onLocalTerminalPeerCancel`, `onTerminalTransportProbe`, `onTerminalDirectRetire`.
   pub trait DirectTerminalPort: Send + Sync + std::fmt::Debug {
       fn peer_offer(&self, request: DLocalTerminalPeerOffer, budget: RequestBudget, fence: LinkFence)
           -> OwnerFuture<Result<WLocalTerminalPeerAnswer, TerminalPeerOfferFailure>>;
       fn peer_cancel(&self, request: &DLocalTerminalPeerCancel);
       fn transport_probe(&self, request: &DTerminalTransportProbe) -> Option<WTerminalTransportProbeResult>;
       fn direct_retire(&self, request: &DTerminalDirectRetire);
   }
   ```
   and field `pub direct: Option<Arc<dyn DirectTerminalPort>>,` on `DownstreamOwners`.
2. `src/runtime/downstream/mod.rs` — add `mod direct;` next to `mod owner_task;` and replace these arms (verbatim old → new):
   - `CoordWorkerDownstream::LocalTerminalPeerOffer(request) => { link.reply(replies::terminal_peer_disabled(&request, &self.process_epoch)); }`
     → `CoordWorkerDownstream::LocalTerminalPeerOffer(request) => self.terminal_peer_offer(request, received, link),`
   - `CoordWorkerDownstream::TerminalTransportProbe(_) => unowned(kind, "W-PEER"),` → `CoordWorkerDownstream::TerminalTransportProbe(request) => self.terminal_transport_probe(&request, link),`
   - `CoordWorkerDownstream::LocalTerminalPeerCancel(_) => unowned(kind, "W-PEER"),` → `CoordWorkerDownstream::LocalTerminalPeerCancel(request) => self.terminal_peer_cancel(&request),`
   - `CoordWorkerDownstream::TerminalDirectRetire(_) => unowned(kind, "W-PEER"),` → `CoordWorkerDownstream::TerminalDirectRetire(request) => self.terminal_direct_retire(&request),`
   (`LocalAttachmentPeerCancel(_) => unowned(kind, "W-PEER")` is WAttachDirect's, leave it to them.) Update the comment above the unowned arms (drop the direct-terminal line refs).
3. `src/runtime/downstream/replies.rs` — replace `terminal_peer_disabled` with:
   ```rust
   /// The fields a `local-terminal-peer-error` echoes, held past the await.
   #[derive(Debug, Clone)]
   pub(super) struct PeerErrorKey { pub(super) request_id: String, pub(super) connection_generation: String, pub(super) peer_id: String }
   impl From<&DLocalTerminalPeerOffer> for PeerErrorKey { fn from(r: &DLocalTerminalPeerOffer) -> Self { Self { request_id: r.request_id.clone(), connection_generation: r.connection_generation.clone(), peer_id: r.peer_id.clone() } } }
   /// v2 `sendPeerError`.
   pub(super) fn terminal_peer_error(key: &PeerErrorKey, worker_epoch: &str, reason: &str) -> CoordWorkerUpstream {
       CoordWorkerUpstream::LocalTerminalPeerError(WLocalTerminalPeerError { request_id: key.request_id.clone(), connection_generation: key.connection_generation.clone(), worker_epoch: worker_epoch.to_owned(), peer_id: key.peer_id.clone(), reason: reason.to_owned(), ..Default::default() })
   }
   ```
   (`PEER_DISABLED` stays only if `attachment_peer_disabled`/WAttachDirect still uses it; the terminal arm uses `TerminalPeerOfferFailure::Disabled.as_str()`.) Update the file's `//!` if it names `sendPeerError` users.
4. `src/runtime/boot.rs` (APPROVED by lead) — `use crate::peer::PeerTransportConfig;`; field on `WorkerBoot`:
   `/// v2 terminalPeer{Enabled,BindAddress,PortRange}; shared by both peer owners.` `pub terminal_peer: PeerTransportConfig,`;
   in `resolve`: `terminal_peer: PeerTransportConfig::resolve(env, platform).map_err(|error| BootConfigError::TerminalPeer(error.0))?,`;
   variant `#[error("{0}")] TerminalPeer(&'static str),` on `BootConfigError`. (WorkerBoot is built only via `resolve`; check `tests/*_support` still compile.)
5. `src/runtime/capabilities.rs` (APPROVED) — `pub fn advertised(direct: crate::peer::DirectPeerSupport) -> Vec<String>`: push
   `CAPABILITY_TERMINAL_PEER_WEBRTC_V1` if `direct.terminal`, `CAPABILITY_ATTACHMENT_TRANSFER_PEER_WEBRTC_V1` if `direct.attachment`
   (v2 main.ts:183-184), then sort+dedup as now. Rewrite the "WHAT IS NOT HERE" doc bullet for the peer capabilities
   (advertised iff the owner's bootstrap is ready). Tests: existing ones call `advertised(DirectPeerSupport::default())`;
   replace `a_capability_with_no_collaborator_behind_it_is_not_advertised` with: default → neither advertised;
   `{terminal: true, attachment: false}` → only the terminal one; `{true, true}` → both, still sorted.
6. `src/runtime/link_loop.rs` — field `pub(super) direct_peers: crate::peer::DirectPeerSupport,` on `LinkLoop`
   (init `DirectPeerSupport::default()` in `LinkLoop::new`) + setter next to `attach_owners`:
   `pub fn attach_direct_peers(&mut self, support: DirectPeerSupport) { self.direct_peers = support; tracing::info!(?support, "the hello advertises the direct peers that bootstrapped"); }`
   `src/runtime/link_serve.rs:110` → `crate::runtime::capabilities::advertised(loop_state.direct_peers)`.
7. `src/runtime/owners.rs` — `WorkerOwners::build(.., transport: PeerTransportConfig)` (plus WDoorTerm's `worker_fingerprint`):
   after WDoorTerm's `let local_terminal = LocalTerminalDoor::new(..)`:
   ```rust
   // v2 main/boot-local-terminal: ONE native load shared by both peer owners.
   let native_loader = crate::peer::native::str0m_loader();
   let direct = DirectTerminal::new(DirectTerminalDeps { door: Arc::clone(&local_terminal), process_epoch: process_epoch.to_owned(),
       transport, native_loader: native_loader.clone(), offer_faults: None, runtime: tokio::runtime::Handle::current() });
   ```
   `DownstreamOwners { .., direct: Some(Arc::clone(&direct) as Arc<dyn DirectTerminalPort>), local_terminal: Arc::clone(&direct) as Arc<dyn LocalTerminalGrantPort> /* was local_terminal.clone(): DirectTerminal adds the peer revoke, v2 wiring.revokeDevice */,
   lifecycle: Arc::new(DirectLinkLifecycle::new(Arc::clone(&direct), Arc::new(cadence.clone()))) as Arc<dyn LinkLifecyclePort> }`;
   fields `pub direct: Arc<DirectTerminal>`, `pub native_loader: NativeLoader` (WAttachDirect reads it + `direct.generation()`, calls `direct.register_carrier(..)`);
   method `pub async fn direct_peer_support(&self) -> DirectPeerSupport { DirectPeerSupport { terminal: self.direct.peer_owner().bootstrap().await == PeerBootstrapState::Ready, attachment: false /* WAttachDirect sets */ } }`;
   `shutdown`: `self.direct.dispose_direct();` REPLACES WDoorTerm's `local_terminal.dispose()` (dispose_direct disposes the door once; door.dispose is idempotent).
8. `src/runtime/boot_sequence.rs` (386 lines; stay ≤400) — pass `boot.terminal_peer` to `WorkerOwners::build`, and after
   `link.attach_owners(owners.downstream.clone());` add `link.attach_direct_peers(owners.direct_peer_support().await);`.
9. `tests/link_downstream_support/mod.rs` — `direct: None,` in `Fakes::owners()`; any other `DownstreamOwners {..}` literal in tests gets it too
   (`grep -rn "DownstreamOwners {" crates/roost-worker`).
10. `crates/roost-worker/README.md` (lead) — module list: `peer/` + `peer/native/`.

## 3. Tests (port → Rust file) and how to run
| v2 test | Rust test (file) |
|---|---|
| terminal-peer-owner.test.ts: binds tuple before SDP / bootstrap states + bounded pending / final slot reserved / no promotion after close / retire one peer | `tests/terminal_peer_owner.rs` (5 tests, same names snake_cased) |
| terminal-peer-test-faults.ts offer faults | `terminal_peer_owner.rs::every_offer_fault_fires_once_at_its_owner_boundary` |
| terminal-peer-packet-port.test.ts (8 tests) | `tests/terminal_peer_packet_port.rs` (8 + `a_send_beyond_the_peer_control_ceiling_is_refused`) |
| coord-link-direct-terminal.test.ts test 1 (terminal half) | `tests/link_downstream_direct.rs` (+ refusal reasons, fence drop) |
| plan: str0m↔str0m pair carries a terminal frame | `tests/terminal_peer_str0m.rs` |
| unit | `stun.rs`, `host_addresses.rs`, `gather.rs`, `config.rs`, `request_validation.rs`, `coordinator_generation.rs` `#[cfg(test)]` |
Run: `/tmp/wcargo.sh test -p roost-worker --test terminal_peer_owner --test terminal_peer_packet_port --test terminal_peer_str0m --test link_downstream_direct --test link_downstream_absent`, `/tmp/wcargo.sh test -p roost-worker --lib peer::`, `--lib capabilities`; then clippy `-p roost-worker --all-targets -- -D warnings`.
`local-terminal-peer-socket.test.ts` is WDoorTerm's. Not ported (v2 has no reader): `capabilityState`, `bufferedBytes()`.
Str0m risk to check first in `terminal_peer_str0m`: negotiated channels are created via `direct_api().create_data_channel`
BEFORE `sdp_api().accept_offer` (v2 order, `native/factory.rs`). If no `ChannelOpen` arrives, move channel creation into
`peer_handle.rs::answer` right after `accept_offer` (still before the driver starts, so no frame can race).

## 4. Mutations to run (mutate, see the named test fail, revert; report file:line)
1. `packet_lanes.rs` flush: call `fragment.commit()` only when `sent_now` → `commits_a_native_false_return_once…` fails (FAILURE-INDEX "node-datachannel sendMessageBinary(false)").
2. `packet_budget.rs` `DirectionBudget::reserve`: drop the worker application cap → `keeps_control_reservation_separate…` fails.
3. `packet_budget.rs` `reserve`: skip the pressure-handler loop → `retires_a_history_holder…` fails.
4. `history_reservation.rs` `reserve_queued`: do not release the transferred reservation → `transfers_a_pre_read_history_ceiling…` fails.
5. `owner_offer.rs` admit: remove `pending.len() >= TERMINAL_PEER_MAX_NEGOTIATIONS_PER_WORKER` → `reports_bootstrap_states_and_bounds_pending_offers` fails.
6. `owner_offer.rs` admit: IdentityMismatch leaves the epoch → `every_offer_fault_fires_once…` fails.
7. `packet_lanes.rs` receive: skip the `channel != Control` check → `refuses_non_control_client_data…` fails.
8. `runtime/downstream/direct.rs`: `uplink.send(frame)` instead of `send_fenced` → `an_answer_for_a_superseded_coordinator_connection_is_dropped` fails.
9. `native/driver.rs` `send_transmit`: return before `send_to` → `terminal_peer_str0m` fails (no DTLS).

## 5. Interfaces agreed with siblings (by message; do not change without telling them)
- WDoorTerm: owns `crate::local_terminal::port` traits (see §0). `open_peer_port(&self, Arc<dyn PeerTerminalPacketPort>, ExpectedPeer) -> PeerIngress`;
  my port calls ingress `on_message`/`on_close` only AFTER releasing the turn; `port.close()` calls `on_close` synchronously
  before returning; `wait_for_lane_drain` resolves on drain AND on close; reservation release = Drop. Grants: `authorize_peer(grant_id, device_fp, tab_id, worker_epoch) -> PeerGrantAuthorization`,
  `remove(grant_id, GrantRemovalReason::Expired)`. WDoorTerm keeps `downstream.local_terminal = local_terminal.clone()` and shutdown `local_terminal.dispose()` until this slice swaps them (§2.7).
- WAttachDirect: uses `peer::native::*` exactly as in `native/mod.rs`; `peer::native::str0m_loader()` ONE instance from owners.rs;
  `peer::PeerTransportConfig`; `peer::CoordinatorGeneration` via `direct.generation()`; implements `peer::DirectCarrier { cancel_pending_for_coordinator, dispose }`
  for its bundle and registers it with `direct.register_carrier(..)`; `peer::TerminalPeerPacketIngress`; `TerminalPeerOwner::new(TerminalPeerOwnerDeps{..})`,
  `offer(request, RequestBudget, LinkFence)`, `established_count()`; test fixtures `tests/peer_support/{fake_native,offerer}.rs` (API in those files). WAttachDirect sets `DirectPeerSupport.attachment`.

## 6. Lead decisions
- APPROVED: `WorkerBoot.terminal_peer: PeerTransportConfig` (§2.4) and `DirectPeerSupport` into `advertised()` (§2.5-2.6).
- Report the replaced arms to the lead (kind, old verbatim reply, new owner): LocalTerminalPeerOffer (`replies::terminal_peer_disabled` → `DirectTerminal`), TerminalTransportProbe / LocalTerminalPeerCancel / TerminalDirectRetire (`unowned(kind, "W-PEER")` → `DirectTerminal`).

## 7. Open parity gaps (for the report)
- The disposable smoke-worker socket that ARMS `OfferFault` (v2 `smoke/terminal/stack-peer-fault-worker-client.ts`) has no Rust entry point; production passes `offer_faults: None`. Other v2 test faults (packet blackhole, malformed packet, history pause/hold, input-result drop, direct-retire drop, grant clock, admission hold) are not ported, by the plan.
- str0m has no ICE `failed` state: `IceConnectionState::Disconnected` is mapped to `Failed` (v2 failed only on libjuice's terminal failure).
- SCTP send/recv buffers and max chunks (v2 `setSctpSettings` 256K/512K/2048) are fixed in str0m (128 KiB send across streams); libdatachannel's `bufferedAmount` is modelled as bytes str0m's SCTP has not accepted yet.
- str0m advertises `max-message-size` 256 KiB; the worker enforces 16 KiB on send only (framing already caps packets at 16 KiB).
- Host candidates: one socket per non-loopback, non-link-local address (loopback only when nothing else); remote mDNS `.local` candidates are ignored by str0m (connectivity via peer-reflexive checks).
- `attachments/attachment-peer-*.ts` (listed under W-PEER in the plan table) are ported by WAttachDirect on this driver.
