worker: WebRTC terminal peer over str0m and the direct-terminal arms

Ports v2 apps/worker/src/terminal/peer/* — terminal-peer-owner.ts (offer
admission, bootstrap, cancel/revoke/detach/dispose; OfferFault injection
sites only from terminal-peer-test-faults.ts), terminal-peer-connection.ts,
terminal-peer-packet-port.ts, terminal-peer-packet-budget.ts,
terminal-peer-history-reservation.ts, terminal-peer-request-validation.ts,
terminal-peer-native.ts (node-datachannel surface over one shared str0m
driver: crate::peer::native, also used by the attachment peer) — plus the
terminal half of transport/coord-link-direct-{terminal,deps}.ts, the peer
half of boot/boot-local-terminal.ts (useCoordinatorGeneration,
disposeDirect, revokeDevice, peerSupported) and host/config.ts
parseTerminalPeer{Enabled,BindAddress,PortRange}.

Replaced downstream arms (runtime/downstream/mod.rs → direct.rs):
- localTerminalPeerOffer: was replies::terminal_peer_disabled → DirectTerminal
  (answer/refusal fenced to the offer's connection; no owner still answers
  `disabled`).
- terminalTransportProbe / localTerminalPeerCancel / terminalDirectRetire:
  were unowned(kind, "W-PEER") → DirectTerminal.

Consumers: DirectTerminalPort ← runtime::downstream::direct; DirectPeerSupport
← capabilities::advertised via LinkLoop::attach_direct_peers (hello);
PeerTransportConfig ← WorkerBoot.terminal_peer → WorkerOwners::build →
DirectTerminal (and the attachment peer owner); DirectLinkLifecycle ←
DownstreamOwners.lifecycle (onDetach clears the coordinator generation
before the session half); DirectTerminal as LocalTerminalGrantPort adds the
peer revoke to wiring.revokeDevice; WorkerOwners::shutdown → dispose_direct.

Mutations (each seen to fail, then reverted; run in a private copy):
M1 packet_lanes commit only when sent_now → commits_a_native_false_return…;
M2 packet_budget drop worker application cap → keeps_control_reservation…;
M3 packet_budget skip pressure handlers → retires_a_history_holder…;
M4 history_reservation keep transferred bytes → transfers_a_pre_read…;
M5 owner_offer drop negotiation bound → reports_bootstrap_states_and_bounds…;
M6 owner_offer identity_mismatch keeps epoch → every_offer_fault_fires_once…;
M7 packet_lanes accept non-control data → refuses_non_control_client_data…;
M8 downstream/direct send unfenced → an_answer_for_a_superseded…;
M9 native/driver never sends → an_in_process_str0m_pair…;
M10 capabilities always terminal → a_peer_capability_is_advertised_only…;
M11 probe any epoch → a_transport_probe_is_answered_only…;
M12 retire any reason → a_direct_retire_disposes_only…;
M14 port range below 1024 → port_ranges_are_bounded_decimal_pairs;
M15 STUN port un-XORed → a_binding_success_yields_its_xor_mapped_address;
M16 uuid version unchecked → a_peer_id_must_be_a_versioned_uuid…;
M17 generation always matches → one_generation_is_adopted_until_cleared +
one_coordinator_generation_is_adopted_per_link…

Shared edits: link_ports.rs (DirectTerminalPort, DownstreamOwners.direct),
runtime/{boot,capabilities,link_loop,link_serve,owners,boot_sequence}.rs,
runtime/downstream/{mod,replies}.rs, tests/link_downstream_support/mod.rs,
tests/link_downstream_live.rs. No Cargo.toml/Cargo.lock change (str0m and
tokio were already dependencies); no cross-crate edit.

Mutation batch at resume (WorkerLead; mutbatch.py: co-applied mutants never share a file
or a test binary; KILLED = the named guard failed under the mutant; every file restored):
- M4 roost-worker peer/history_reservation.rs: `if reserved != 0 { self.base.clone().release(reserved);` -> `if reserved != 0 { let _ = reserved;` => SURVIVED (named guard * stayed green) — open, see handoff
- M3 roost-worker peer/packet_budget.rs: `for (id, handler) in handlers {` -> `for (id, handler) in handlers.into_iter().take(0) {` => FAILS retires_a_history_holder_instead_of_the_healthy_terminal_sender_under_worker_pressure
- M6 roost-worker peer/owner_offer.rs: `format!("{}-smoke-mismatch", request.worker_epoch)` -> `request.worker_epoch.clone()` => FAILS every_offer_fault_fires_once_at_its_owner_boundary
- M7 roost-worker peer/packet_lanes.rs: `if channel != TerminalPeerPacketLane::Control as usize || !binary {` -> `if !binary {` => FAILS refuses_non_control_client_data_before_it_can_reach_a_terminal_ingress
- M17 roost-worker peer/coordinator_generation.rs: `Some(adopted) => adopted == generation,` -> `Some(_) => true,` => SURVIVED (named guard * stayed green) — open, see handoff

Test fix at resume: the packet-port tests import the TerminalPacketPort /
PeerTerminalPacketPort / TerminalPeerPacketQuota traits the split moved the methods onto
(all reachable from roost_worker::local_terminal and roost_protocol::terminal_peer::packets).

Survivors at resume:
- M4 (history_reservation.rs untransferred read never returned to the base quota): the old guard
  released only on a CLOSED port, where finish_dispose hands every byte back (v2
  terminal-peer-history-reservation.ts:47-57 releases to baseQuota). New guard
  terminal_peer_packet_port::an_untransferred_read_returns_its_ceiling_to_an_open_port FAILS under it.
- M17 (coordinator_generation.rs any generation accepted): not a survivor — the batch ran only
  integration bins; --lib peer::coordinator_generation::tests::one_generation_is_adopted_until_cleared
  FAILS under it (v2 boot-local-terminal.ts:140).

Series: one of the wave 2+3 per-slice commits (ProtoDoor..WAgentsPrompt). A file shared by
several slices lands whole in the FIRST slice commit that lists it, so intermediate commits
may not build; the track gate (tests x2, clippy, lint, fmt) ran on the series tip.
