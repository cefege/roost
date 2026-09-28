worker: local terminal door — grants, prehello, direct sockets and scrollback

Lands `crate::local_terminal`, the worker's direct terminal path: the
coordinator-installed grant store (SHA-256 digests only, active expiry,
epoch fencing), the socket owner that admits loopback and WebRTC packet
ports against it (Hello admission, peer tuple match, grant replacement,
live per-session authorization re-evaluated at every keeper boundary),
direct input/route-claim/probe/history controls, cell and view delivery
through the shared TerminalViewOwner, and the direct scrollback reader
over the one shared page reader. `local_door.rs` is rewritten to v2's
pre-hello owner (deadline, established cap, grant replacement); grants
live in their own store as in v2.

v2 ported: apps/worker/src/local-door/{local-terminal-socket,
local-terminal-socket-authority,-controls,-delivery,-input,
local-terminal-scrollback,local-terminal-grants,local-terminal-prehello,
local-ui-terminal-socket}.ts, boot/boot-local-terminal.ts (owner half),
terminal/peer/terminal-packet-port.ts, and the localTerminalGrant /
localTerminalGrantRevoke cases of transport/coord-link-downstream.ts.

Downstream arms replaced:
- local-terminal-grant: was rpc-error "local terminal grants unsupported
  by this worker" → LocalTerminalGrantPort::install_grant (rpc-ok
  {grant_id} / rpc-error <store refusal>); the absent reply stays only for
  a Dispatcher built without owners.
- local-terminal-grant-revoke: was the no-frame warn arm → 
  LocalTerminalGrantPort::revoke_device (routes, then grants, whose
  removal closes the device's sockets).

Consumers: DownstreamOwners.local_terminal is read by
runtime::downstream::local_grants; WorkerOwners.local_terminal by
runtime::owners (door router: LoopbackOwner::terminal(sockets()), peer
DirectTerminal); SessionTable::session_of_channel by
local_terminal::delivery::PortViewTransport; retained_grid::row_cells by
local_terminal::scrollback and the browser page reader.

Shared/cross-owner edits: link_ports.rs (LocalTerminalGrantPort + field),
runtime/owners.rs (door composition, shutdown dispose first),
runtime/boot_sequence.rs (fingerprint arg), runtime/downstream/mod.rs
(2 arms + mod), session/table.rs (session_of_channel),
session/retained_grid.rs (row_cells), scrollback_read.rs (walk_page asks
continue_read only before a slice with rows, v2
browser-command-terminal.ts:115-117), Cargo.toml dev tokio test-util,
tests/link_downstream_support (fake port), tests/link_downstream_absent
(dropped the two assertions that pinned the replaced absent-owner arms).
Deleted tests/local_door.rs: it pinned the pre-v2 paraphrased API;
replaced by ported local_terminal_prehello + local_terminal_grants.

Mutations: see WDoorTerm report (each guarded product line mutated, its
test seen failing, reverted).

Mutation batch at resume (WorkerLead; mutbatch.py: co-applied mutants never share a file
or a test binary; KILLED = the named guard failed under the mutant; every file restored):
- M1 roost-worker local_terminal/grants.rs: `if capability_matches(installed, &sha256_hex(credential.secret)) {` -> `if capability_matches(installed, installed) {` => FAILS a_hello_whose_secret_does_not_match_is_closed_and_registers_nothing
- M5 roost-worker local_door.rs: `|| state.timers.len() >= TERMINAL_PEER_MAX_ESTABLISHED_PER_WORKER` -> `|| state.timers.len() > TERMINAL_PEER_MAX_ESTABLISHED_PER_WORKER` => FAILS caps_and_expires_loopback_sockets_awaiting_hello
- M8 roost-worker local_terminal/authority.rs: `.any(|granted| granted == session_id)` -> `.any(|granted| !granted.is_empty())` => SURVIVED (named guard input_takes_the_real_write_path_and_reports_its_outcome_per_session stayed green) — open, see handoff
- M10 roost-worker local_terminal/hello.rs: `return hello.peer_id.is_empty() && hello.worker_epoch.is_empty();` -> `return true;` => FAILS loopback_rejects_a_peer_shaped_hello_instead_of_accepting_old_compatibility
- M12 roost-worker scrollback_read.rs: `if offset >= slice_end { break; } // v2 `browser-command-terminal.t...` -> `if !continue_read() { return WalkOutcome::Cancelled { taken }; } if...` => SURVIVED (named guard post_read_authority_loss_suppresses_the_direct_scrollback_page stayed green) — open, see handoff
- M13 roost-worker runtime/downstream/local_grants.rs: `data: serde_json::json!({ "grant_id": request.grant_id }),` -> `data: serde_json::json!({}),` => FAILS an_accepted_install_is_acknowledged_with_its_grant_id
- M2 roost-worker local_terminal/sockets.rs: `_ if !session.is_authenticated() => self.close(&session, "hello req...` -> `_ if !session.is_authenticated() => {}` => FAILS any_frame_before_the_hello_closes_the_socket
- M7 roost-worker local_terminal/grants.rs: `fn expire_install(&self, grant_id: &str, install: u64) {` -> `fn expire_install(&self, grant_id: &str, install: u64) { if install...` => FAILS renewal_grows_in_place_reports_scope_reduction_and_actively_expires
- M9 roost-worker local_terminal/delivery.rs: `} => ServerFrame::from(InputAmbiguous { session_id, input_seq, doma...` -> `} => ServerFrame::from(InputRejected { session_id, input_seq, domai...` => FAILS a_written_batch_whose_answer_is_lost_is_ambiguous
- M11 roost-worker local_terminal/authority.rs: `(Some(_), true) => false,` -> `(Some(_), true) => true,` => FAILS peer_hello_matches_the_offer_tuple_before_it_becomes_authenticated
- M3 roost-worker local_terminal/sockets.rs: `GrantChange::Removed { grant, .. } => (&grant.grant_id, "local term...` -> `GrantChange::Removed { .. } => return,` => FAILS revoking_the_device_closes_its_socket
- M6 roost-worker local_door.rs: `(shared.on_timeout)(socket_id);` -> `let _ = &shared.on_timeout;` => FAILS a_socket_that_never_says_hello_is_closed_at_the_deadline
- M8b roost-worker local_terminal/authority.rs: `.any(|granted| granted == session_id)` -> `.any(|granted| !granted.is_empty())` => SURVIVED (named guard a_view_for_a_session_outside_the_grant_is_refused stayed green) — open, see handoff
- M14 roost-worker session/table.rs: `.find(|(_, channel)| **channel == channel_id)` -> `.find(|(_, channel)| **channel != channel_id)` => FAILS a_granted_socket_receives_cells_for_its_view
- M15 roost-worker local_terminal/delivery.rs: `PacketSendResult::Refused => { close(); CellSinkResult::Overflow }` -> `PacketSendResult::Refused => { let _ = close; CellSinkResult::Sent }` => FAILS a_socket_that_stops_draining_is_dropped_alone

Red classified at resume: local_terminal_socket::a_socket_that_stops_draining_is_dropped_alone
was a FIXTURE defect — CellCadence::new registers an unattached CoordinatorCellSink as "coord",
which holds the stream baseline so no delta reached local sinks; v2's harness
(terminal-stream-state-harness.ts:92-96) registers an accepting "coord" sink, so the fixture
re-registers the harness's RecordingSink after CellCadence::spawn. No assert changed; M15
(delivery.rs Refused close) now fails it.

Survivors at resume, guards added (pass on real code, FAIL under the mutant):
- M8/M8b (authority.rs `granted == session_id` -> any non-empty grant): the old guards probed a
  session that was also not live, so the liveness check hid membership (v2
  local-terminal-socket-authority.ts:65 needs both). New guard
  local_terminal_socket_input::a_live_session_the_grant_does_not_name_is_refused FAILS under it.
- M12 (scrollback_read.rs cancel check before the empty-slice break; v2
  browser-command-terminal.ts:115-117 asks only before a slice with rows). New guard
  scrollback_read::authority_is_asked_only_before_a_slice_that_has_rows FAILS under it.

Series: one of the wave 2+3 per-slice commits (ProtoDoor..WAgentsPrompt). A file shared by
several slices lands whole in the FIRST slice commit that lists it, so intermediate commits
may not build; the track gate (tests x2, clippy, lint, fmt) ran on the series tip.
