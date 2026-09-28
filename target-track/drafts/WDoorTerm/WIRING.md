# WDoorTerm — phase-2 wiring handbook (self-sufficient)

Slice: W-DOOR terminal half of Stage 2W (`docs/v3-handoff/roost-v3-finish-and-cutover-plan.md` "### Stage 2W").
Worktree `/home/almalinux/repos/roost-v3-worker`, crate `crates/roost-worker` unless named. v2 authority:
`apps/worker/src/local-door/*.ts`, `apps/worker/src/boot/boot-local-terminal.ts`, tests in
`apps/worker/tests/local-door/*.test.ts`. v2 wins wherever this handbook and v2 disagree.
Drafts root: `target-track/drafts/WDoorTerm/` (mirrors repo paths). Nothing here has been compiled yet:
expect small API mismatches; fix them against the real signatures, never by changing a sibling's seam.

## 0. Rules recap for the phase-2 agent
Build only via `/tmp/wcheck.sh '<regex>'` (check) and `/tmp/wcargo.sh test|clippy ...`; files ≤400 lines;
`//!` header 3–6 lines naming v2 file + caller; no unwrap/expect outside tests; tracing per transition;
never commit/fmt/stash. Every new guard: mutate the guarded product line, see the test fail, revert, record.
Write `target-track/wave-gates/done-WDoorTerm` (DONE or BLOCKED+reason) at the end and yield the report
format from the lead's brief (files+line counts, v2 files ported, tests pass/fail, mutations, consumers,
cross-owner edits, shared-file edits, parity gaps, commit message draft + path list).
Report the replaced downstream arms to the lead (kind, old verbatim reply, new owner) — do NOT edit the
arm→owner table in `docs/v3-handoff/worker-lead-handoff.md`.

## 1. Draft files → move into place (all new unless noted)

| draft path (under drafts/WDoorTerm/) | ports v2 | purpose |
|---|---|---|
| `crates/roost-worker/src/local_terminal/mod.rs` | – | module root: `mod` lines + `pub use` re-exports |
| `.../local_terminal/port.rs` | `terminal/peer/terminal-packet-port.ts` (+ peer port / expected tuple types) | `PacketSendResult`, `TerminalPacketPort`, `PeerTerminalPacketPort`, `HistoryReadReservation`, `ExpectedPeer` |
| `.../local_terminal/grant_scope.rs` | types + validators of `local-terminal-grants.ts`, `verifyLocalEndpointCapability` (`packages/host/src/local-endpoint.ts`) | `LocalTerminalGrant`, `GrantCredential`, `GrantRemovalReason{Expired,Revoked,Disposed}`, `GrantChange{Installed,Renewed{removed_session_ids},Removed{reason}}`, `PeerGrantAuthorization`, `valid_id`, `valid_session_ids`, `is_sha256_hex`, `sha256_hex`, `capability_matches` (constant-time) |
| `.../local_terminal/grants.rs` | `local-terminal-grants.ts` | `LocalTerminalGrantStore` (Clone, Arc inside): `new(epoch, Handle)`, `subscribe/unsubscribe`, `install(&DLocalTerminalGrant)->Result<Arc<Grant>,String>`, `current`, `verify`, `authorize_peer`, `revoke_device`, `remove`, `dispose`; per-install tokio expiry timer; listeners called AFTER the lock is dropped |
| `.../local_terminal/authority.rs` | `local-terminal-socket-authority.ts` | `Carrier{Loopback,Peer{port,expected}}`, `PortSession` (identity mutex + `closing` atomic, `begin_close`), `PortRegistry`, `AuthorizationDeps`, `direct_port_actor`, `is_direct_port_session_authorized`, `DirectPortBudget` (TerminalWriteBudget), `DirectClaimBudget` (RouteClaimBudget), `DirectPortAuthority` (TerminalWriteAuthority) |
| `.../local_terminal/sockets.rs` | `local-terminal-socket.ts` | `LocalTerminalSockets` (Arc::new_cyclic): `on_open`, `open_peer_port -> PeerIngress`, `on_message`, `on_close`, `revoke_device`, `dispose`; `close` (inline) vs `close_deferred` (fence now, teardown spawned); grant-change listener |
| `.../local_terminal/hello.rs` | `accept`/`matchesExpectedPeer` of `local-terminal-socket.ts` | Hello admission, ready frame, `TerminalViewOwner::register_local` |
| `.../local_terminal/input.rs` | `local-terminal-socket-input.ts` | `start_input`: sync prefix (authorized → size → work budget) in receive order, write future awaited on a spawned task, reservation dropped after the answer |
| `.../local_terminal/controls.rs` | `local-terminal-socket-controls.ts` | `start_claim`, `probe`, `start_scrollback` (+ per-port history lock, loopback history reservation, peer history reservation/drain) |
| `.../local_terminal/scrollback.rs` | `local-terminal-scrollback.ts` | `pub async fn read_local_scrollback(manager, table, request, allows)` over `scrollback_read::{page_for, walk_page}` + `retained_grid::{describe_grid, row_cells}` + `history_floor_for` |
| `.../local_terminal/delivery.rs` | `local-terminal-socket-delivery.ts` + `transport()` of `local-terminal-socket.ts` | frame encode/send, input-result frames, cell delivery mapping, `PortViewTransport` (impl `terminal_view::LocalViewTransport`) |
| `.../local_terminal/loopback.rs` | `local-ui-terminal-socket.ts` | `LoopbackTerminalPacketPort` over WDoorHttp's `door::LoopbackSocket`; `impl door::loopback::LoopbackHandlers for LocalTerminalSockets` |
| `.../local_terminal/door.rs` | owner half of `boot/boot-local-terminal.ts` | `LocalTerminalDoor::new(LocalTerminalDoorDeps)`, `sockets()`, `grants()`, `worker_epoch()`, once-guarded `dispose()` (= v2 disposeDirect minus peer/attachment), `impl LocalTerminalGrantPort` |
| `crates/roost-worker/src/local_door.rs` | `local-terminal-prehello.ts` | **REPLACES the existing file** (the existing one conflates grants+prehello with non-v2 caps). New API: `LocalTerminalPreHelloOwner::new(PreHelloTimeout, Handle)`, `admit(&str)->bool`, `authenticate(grant,socket)->AuthenticatedAdmission{admitted, replaced_socket_id}`, `clear`, `retire`, `dispose`. Constants come from `roost_protocol::terminal_peer::peer::{TERMINAL_PEER_HELLO_DEADLINE_MS, TERMINAL_PEER_MAX_ESTABLISHED_PER_WORKER}`. Old `sha256_of`/`PREHELLO_DEADLINE`/`MAX_ESTABLISHED`/`SocketId` are gone — their only caller is `tests/local_door.rs` (deleted, §3). `roost-client-core/src/client/local/door.rs:14` doc still names `local_door.rs` for the replace rule — still true, no edit. |
| `crates/roost-worker/src/runtime/downstream/local_grants.rs` | `localTerminalGrant`/`localTerminalGrantRevoke` cases of `transport/coord-link-downstream.ts:269-300` + callbacks `coord-link-deps.ts:128-135` | `impl Dispatcher { fn local_terminal_grant(..), fn local_terminal_revoke(..) }` |
| `crates/roost-worker/tests/local_terminal_support/mod.rs` | fixture of `local-terminal-socket.test.ts` (`installAutoKeeper`, stub socket) | `AutoKeeper` (KeeperChannels: resize Applied, input Ack/scripted), `StubPort` (+`open_on`), `Fixture` (stream harness + CellCadence + SessionViewPort + TerminalViewOwner + LocalTerminalDoor, grant installed), frame builders, `settle()` |
| `tests/local_terminal_socket.rs` | `local-terminal-socket.test.ts` (secret mismatch, frame-before-hello, ready/replaced/duplicate, view outside grant, revoke, stalled dropped alone) + unknown-grant refusal + granted socket receives cells + prehello deadline through the owner | |
| `tests/local_terminal_socket_input.rs` | input case of `local-terminal-socket.test.ts` + oversized + ambiguous | accepted/rejected/ambiguous |
| `tests/local_terminal_pty.rs` | – (acceptance: input reaches the PTY) | real keeper + real PTY through `WorkerOwners::build` |
| `tests/local_terminal_grants.rs` | `local-terminal-grants.test.ts` + verify order + install refusals + revoke | |
| `tests/local_terminal_prehello.rs` | `local-terminal-prehello.test.ts` (both tests) | paused tokio clock |
| `tests/local_terminal_peer_socket.rs` | `local-terminal-peer-socket.test.ts` (both tests) | fake `PeerTerminalPacketPort` |
| `tests/local_terminal_scrollback.rs` | `local-terminal-scrollback.test.ts` + one positive page | |
| `tests/link_downstream_local_grant.rs` | the two downstream cases | rpc-ok `{grant_id}`, rpc-error reason, absent owner, revoke routed |

Test support note: test files that `mod local_terminal_support;` must also `mod terminal_stream_support;`
(the support module does `use crate::terminal_stream_support::{COLS, Harness, ROWS, SESSION, held}`).
`local_terminal_pty.rs` additionally needs `mod keeper_pool_support;`.

## 2. Exact edits to existing files (re-read each file right before editing; siblings edit concurrently)

### 2.1 `crates/roost-worker/src/lib.rs`
After the line `pub mod local_door;` add `pub mod local_terminal;`.

### 2.2 `crates/roost-worker/src/link_ports.rs` (shared; W-DOWN seam)
- Add `DLocalTerminalGrant` to the `use roost_proto::{..}` list.
- Before `/// Every owner a downstream frame can route to.` add:
```rust
/// The local terminal door's grant half. v2 `onLocalTerminalGrant` /
/// `onLocalTerminalGrantRevoke`. Implemented by `local_terminal::LocalTerminalDoor`
/// (WPeer may later wrap it in `peer::DirectTerminal`).
pub trait LocalTerminalGrantPort: Send + Sync + std::fmt::Debug {
    /// Install or renew a grant; `Err` is the message the install is refused with.
    fn install_grant(&self, request: &DLocalTerminalGrant) -> Result<(), String>;
    /// Fence a device: its input routes, then its grants (which close its sockets).
    fn revoke_device(&self, device_fingerprint: &str);
}
```
- In `pub struct DownstreamOwners { .. }` add field `pub local_terminal: Arc<dyn LocalTerminalGrantPort>,`
  (after `lifecycle`). Every `DownstreamOwners { .. }` literal must then set it (owners.rs §2.4,
  tests/link_downstream_support §2.9). Grep `DownstreamOwners {` to find any new ones.

### 2.3 `crates/roost-worker/src/runtime/downstream/mod.rs` (shared; W-DOWN)
- Add `mod local_grants;` next to `mod owner_task;` / `mod replies;` / `mod terminal;`.
- Replace the arm (verbatim today):
```rust
            CoordWorkerDownstream::LocalTerminalGrant(request) => link.reply(replies::rpc_error(
                request.request_id,
                replies::LOCAL_TERMINAL_GRANTS_UNSUPPORTED,
            )),
```
  with `CoordWorkerDownstream::LocalTerminalGrant(request) => self.local_terminal_grant(request, link),`
- Replace the arm
```rust
            CoordWorkerDownstream::LocalTerminalGrantRevoke(_) => {
                unowned(kind, "the local terminal grant owner");
            }
```
  with `CoordWorkerDownstream::LocalTerminalGrantRevoke(request) => self.local_terminal_revoke(&request),`
- `replies::LOCAL_TERMINAL_GRANTS_UNSUPPORTED` stays (used by local_grants.rs for `owners == None`).
- Report to lead: arm `local-terminal-grant` old reply `rpc-error "local terminal grants unsupported by this worker"` (always) → owner `LocalTerminalGrantPort` (rpc-ok `{grant_id}` / rpc-error reason; absent-owner reply kept only when the Dispatcher has no owners); arm `local-terminal-grant-revoke` old: debug log "no owner" → owner `LocalTerminalGrantPort::revoke_device`.

### 2.4 `crates/roost-worker/src/runtime/owners.rs` (lead's composition file; allowed shared edit)
- `use crate::local_terminal::{LocalTerminalDoor, LocalTerminalDoorDeps};` and `LocalTerminalGrantPort` in the `crate::link_ports::{..}` import.
- `WorkerOwners` gets field (doc: "v2 `LocalTerminalDoor.wiring`: the grant store and direct socket owner; the door's router serves `local_terminal.sockets()`."):
  `pub local_terminal: Arc<LocalTerminalDoor>,`
- `WorkerOwners::build(stack, uplink, process_epoch, pool)` gains a LAST parameter `worker_fingerprint: &str`.
- After `let view = TerminalViewOwner::new(..);` add:
```rust
        // v2 `boot-local-terminal.ts`: the direct path shares the link's view
        // owner, input routes and work budget rather than owning copies.
        let local_terminal = LocalTerminalDoor::new(LocalTerminalDoorDeps {
            manager: Arc::clone(&stack.manager),
            sessions: Arc::clone(&stack.table),
            view: Arc::clone(&view),
            routes: routes.clone(),
            work_budget: work_budget.clone(),
            worker_fingerprint: worker_fingerprint.to_owned(),
            process_epoch: process_epoch.to_owned(),
            runtime: tokio::runtime::Handle::current(),
        });
```
- In the `DownstreamOwners { .. }` literal: `local_terminal: Arc::clone(&local_terminal) as Arc<dyn LocalTerminalGrantPort>,`
  (WPeer will later swap this to its `DirectTerminal`).
- In `Self { .. }`: `local_terminal,`.
- `shutdown(self)`: call `self.local_terminal.dispose();` FIRST, before `self.view.dispose();` (v2 `close()`:
  server.close → disposeDirect → viewOwner.dispose). `dispose` is once-guarded; routes/work_budget dispose twice harmlessly.
  (WPeer will later replace it with `direct.dispose_direct()`.)
- Keep owners.rs ≤400; it was 182 lines.

### 2.5 `crates/roost-worker/src/runtime/boot_sequence.rs` (lead's; ~386 lines, one-line edit)
In the `super::owners::WorkerOwners::build(stack, &uplink, &boot.process_epoch, Arc::clone(&survivors),)` call
add a last argument `boot.fingerprint.as_str(),`. Fix any other caller of `WorkerOwners::build` (grep tests).
WDoorHttp's door serve uses `owners.local_terminal.sockets()` (their edit, not ours).

### 2.6 `crates/roost-worker/src/session/table.rs` (cross-owner, additive)
After `pub fn channel_of(..)` add:
```rust
    /// The session a keeper channel carries, or `None` when this worker does
    /// not hold it. Leaf lock only: a direct cell frame names its session, and
    /// its sink runs under the emitter's lock.
    pub fn session_of_channel(&self, channel_id: u16) -> Option<SessionId> {
        self.lock()
            .by_session
            .iter()
            .find(|(_, channel)| **channel == channel_id)
            .map(|(session, _)| session.clone())
    }
```
Consumer: `local_terminal::sockets::LocalTerminalSockets::session_of_channel` (delivery.rs `PortViewTransport`).

### 2.7 `crates/roost-worker/src/session/retained_grid.rs` (cross-owner, refactor, one reader)
Replace `fn row_value(record, absolute_row) -> Option<CellRowJson>` body with a call through a new
```rust
/// One row by its absolute index as the value model, or `None` when the grid
/// no longer holds it. The page reader and the direct scrollback reader both
/// read rows through here.
pub(crate) fn row_cells(record: &SessionRecord, absolute_row: u32) -> Option<CellRow> {
    let core = record.terminal_core.as_ref();
    let origin = scrollback_origin(core, record.cell_emit.scrollback_origin).ok()?;
    let index = u64::from(absolute_row);
    read_scrollback_range(core, index, index + 1, origin).into_iter().next()
}
```
and `row_value` becomes `row_cells(record, absolute_row).map(CellRowJson::owned)` (keep its comment about
owned rows). `CellRow` import: `use roost_protocol::cell::{CellRow, CellSpan};` already present.
Consumer: `local_terminal/scrollback.rs`.

### 2.8 `crates/roost-worker/src/scrollback_read.rs` `walk_page` (v2 parity fix; cross-owner)
v2 `browser-command-terminal.ts:115-117` calls `continueRead` only for a slice that has rows. Today's loop calls
`continue_read()` first. Change the loop head to:
```rust
    loop {
        let slice_end = (offset + SCROLLBACK_SLICE_ROWS).min(page.row_count());
        if offset >= slice_end {
            break;
        }
        if !continue_read() {
            return WalkOutcome::Cancelled { taken };
        }
```
(i.e. swap the two blocks). Needed by the ported v2 scrollback test (exactly 2 authority checks for an empty
page). `tests/scrollback_read.rs` must still pass (its cancel test uses a 1000-row page). This is a guard →
mutation (see §5).

### 2.9 `crates/roost-worker/tests/link_downstream_support/mod.rs` (WDown's test support)
- `use roost_worker::link_ports::LocalTerminalGrantPort;` and `use roost_proto::DLocalTerminalGrant;`.
- In `Fakes::owners()` literal add `local_terminal: Arc::clone(&fakes) as Arc<dyn LocalTerminalGrantPort>,`
  (keep `lifecycle: fakes as ..` last, or clone before it).
- Add:
```rust
impl LocalTerminalGrantPort for Fakes {
    fn install_grant(&self, request: &DLocalTerminalGrant) -> Result<(), String> {
        self.log.push(format!("local_terminal.install_grant:{}", request.grant_id));
        if request.grant_id.is_empty() { Err("grant_id is invalid".to_owned()) } else { Ok(()) }
    }
    fn revoke_device(&self, device_fingerprint: &str) {
        self.log.push(format!("local_terminal.revoke_device:{device_fingerprint}"));
    }
}
```
  NOTE the draft test `a_refused_install_answers_the_stores_reason` does not assert the log, fine either way.
  Keep the file ≤400 (263 lines today).

### 2.10 `crates/roost-worker/tests/link_downstream_absent.rs`
Classified against v2 (`coord-link-downstream.ts:269-300`, `coord-link-deps.ts:130-135`): with the local door
present v2 installs/revokes, so these two assertions pinned the absent-owner stub arm this slice replaces.
- In `unsupported_grants_and_keeper_update_answer_v2s_rpc_errors`: delete the two lines building
  `DLocalTerminalGrant` and asserting `"local terminal grants unsupported by this worker"` (that case now lives in
  `tests/link_downstream_local_grant.rs::a_worker_without_a_local_door_refuses_grants_as_v2_does`).
- In `optional_callbacks_and_retired_tags_are_inert`: delete `Down::LocalTerminalGrantRevoke(..)` from `inert`.
- Remove now-unused imports (`DLocalTerminalGrant`, `DLocalTerminalGrantRevoke`) and update the `//!` header if it names them.

### 2.11 `crates/roost-worker/Cargo.toml` `[dev-dependencies]`
`tokio = { workspace = true, features = ["net", "rt", "rt-multi-thread", "macros", "time", "test-util"] }`
(`start_paused` tests; roost-coord already uses `test-util`, so Cargo.lock does not change — verify).

### 2.12 Delete `crates/roost-worker/tests/local_door.rs`
It pins the paraphrased pre-v2 API (u64 socket ids, grants inside the prehello owner, a combined cap).
Replaced by `tests/local_terminal_prehello.rs` + `tests/local_terminal_grants.rs` (ported v2 tests).
Record the deletion + reason in the report.

### 2.13 Not ours (WDoorHttp): `door/mod.rs` doc line saying `crate::local_door` owns grant digests —
WDoorHttp agreed to reword it; grant digests now live in `local_terminal::grants`.

## 3. Interfaces agreed with siblings
- **WDoorHttp** (router, `door/loopback.rs`, `door/loopback_socket.rs`, constants moved to
  `roost_protocol::local_ui_door`, re-exported at `crate::door`): trait `door::loopback::LoopbackHandlers { type Port;
  fn port(&self, socket_id: String, socket: LoopbackSocket) -> Arc<Self::Port>; fn on_open/on_message(bytes: Vec<u8>)/on_close(&self, &Arc<Port>) -> anyhow::Result<()> }`;
  `crate::door::{LoopbackSend{Written,Queued,Dropped}, LoopbackSocket{send,close,is_open}, LOCAL_TERMINAL_MAX_BACKPRESSURE_BYTES}`
  (`door::loopback_socket` is private — import from `crate::door`). WDoorHttp mints socket ids, logs
  opened/closed, drops text frames, runs `LoopbackOwner::terminal(owners.local_terminal.sockets())`, and shuts the
  server before `local_terminal.dispose()`. If `LoopbackHandlers`/`LoopbackSocket` land with other names, adapt
  `local_terminal/loopback.rs` only.
- **WPeer**: implements `local_terminal::{TerminalPacketPort, PeerTerminalPacketPort}` on its peer port;
  calls `door.sockets().open_peer_port(port as Arc<dyn PeerTerminalPacketPort>, ExpectedPeer{..}) -> PeerIngress`
  (`on_message(&[u8])`, `on_close()`), `door.grants().authorize_peer(grant, device, tab, epoch)`,
  `door.grants().remove(id, GrantRemovalReason::Expired)`, `door.worker_epoch()`. Contract: WPeer calls
  ingress only with its own lock released; `port.close()` calls `ingress.on_close()` synchronously (our
  `finish_close` runs our own `on_close` first, so the re-entry is a no-op); `wait_for_lane_drain` resolves on
  drain AND close. WPeer builds `peer::DirectTerminal` holding `Arc<LocalTerminalDoor>`, implementing
  `LocalTerminalGrantPort` (install → door, revoke → `door.revoke_device(fp)` then peer revoke) and swaps
  `downstream.local_terminal` + shutdown to it in its own phase 2. WPeer owns the `TerminalTransportProbe`,
  `TerminalDirectRetire`, `LocalTerminalPeerOffer`, `LocalTerminalPeerCancel` arms. `LocalTerminalDoor::dispose`
  must stay once-guarded (it is).
- Wave-1 seams used (unchanged): `terminal_input::{TerminalInputRouteOwner (claim, is_current,
  allows_legacy_input, retire_connection, revoke_device, dispose), TerminalInputWorkBudget (reserve_input
  Direct{port_id}, dispose), RouteActor, RouteClaim, RouteClaimBudget, InputWorkOrigin}`;
  `session::input_write::{TerminalWriteBudget, TerminalWriteAuthority, WorkerInputResult}` +
  `SessionManager::write_terminal_input(&SessionId, seq, Vec<u8>, Option<Box<budget>>, Option<Box<authority>>)`;
  `TerminalViewOwner::{register_local(LocalViewRegistration), handle_view_command, handle_resync, close_socket}`
  (the view owner registers the local cell sink on `CellCadence` itself); `SessionManager::control_lanes().settled(ChannelId)`.

## 4. Design decisions (made by this slice; flag in the report)
1. `local_door.rs` keeps its path (named seam, referenced by roost-client-core docs and the v2 map) but is
   rewritten to v2 `local-terminal-prehello.ts`; grants are a separate store (`local_terminal::grants`) as in v2.
2. Closes triggered from inside another owner's lock (view transport callbacks, cell sink overflow, grant-change
   listener — lazy expiry inside `grants.current()` can run under the route owner's lock via
   `RouteClaimBudget::is_session_authorized`) use `close_deferred`: the port is fenced synchronously
   (`closing=true`, every predicate fails) and the teardown (closed frame, retire, `port.close`) runs on a
   spawned task. Everything else closes inline as v2. Tests therefore `settle().await` before asserting revoke/overflow.
3. Input/claim/scrollback: the synchronous prefix (checks, reservation, keeper admission) runs inside
   `on_message` in receive order; only the answer is awaited on a spawned task (v2's async functions run
   synchronously until their first await).
4. Not ported (smoke-only source faults): `LocalTerminalSocketTestFaults` (`onAuthenticatedPeerInput`,
   `shouldSendPeerInputResult`, `onPeerHistoryResponse`), grants `_sweepExpiredForTest`/`_shrinkSessionForTest`,
   `clear()`, `count()` (no production reader), `bufferedBytes()`/`kind` on the port (no v2 reader).
5. `LocalTerminalReady.socket_generation`, input results' `domain_generation` = the per-owner generation counter
   (consumer: the browser's direct carrier; wire field).

## 5. Planned mutations (run each: mutate, run the named test, see it FAIL, revert, record)
| file:anchor | mutation | expected failing test |
|---|---|---|
| `local_terminal/hello.rs` `self.authorization.grants.verify(credential)` | replace the `Err(reason)` branch's close with proceeding (or make `capability_matches` return true) | `local_terminal_socket::a_hello_whose_secret_does_not_match_is_closed_and_registers_nothing` |
| `local_terminal/sockets.rs` `_ if !session.is_authenticated() => self.close(..., "hello required")` | delete that arm's close (make it `{}`) | `local_terminal_socket::any_frame_before_the_hello_closes_the_socket` |
| `local_terminal/sockets.rs` `on_grant_change` `GrantChange::Removed { grant, .. } =>` | return early (skip closing) | `local_terminal_socket::revoking_the_device_closes_its_socket` |
| `local_terminal/delivery.rs` `local_terminal_cell_delivery` Refused arm | return `CellSinkResult::Sent` without close | `local_terminal_socket::a_socket_that_stops_draining_is_dropped_alone` |
| `local_door.rs` `admit` cap `>= TERMINAL_PEER_MAX_ESTABLISHED_PER_WORKER` | `>` | `local_terminal_prehello::caps_and_expires_loopback_sockets_awaiting_hello` |
| `local_door.rs` `expire` → `(shared.on_timeout)(socket_id)` | skip the call | `local_terminal_socket::a_socket_that_never_says_hello_is_closed_at_the_deadline` |
| `local_terminal/grants.rs` `arm_expiry` `sleep_until(expires_at)` path / `expire_install` | early return in `expire_install` | `local_terminal_grants::renewal_grows_in_place_reports_scope_reduction_and_actively_expires` |
| `local_terminal/authority.rs` `is_direct_port_session_authorized` `grant.session_ids.iter().any(..)` | drop that clause | `local_terminal_socket_input::input_takes_the_real_write_path...` (UNGRANTED accepted) and `a_view_for_a_session_outside_the_grant_is_refused` |
| `local_terminal/delivery.rs` `input_result_frame` Ambiguous arm | build `InputRejected` | `local_terminal_socket_input::a_written_batch_whose_answer_is_lost_is_ambiguous` |
| `local_terminal/hello.rs` `matches_expected_peer` loopback branch | return true | `local_terminal_peer_socket::loopback_rejects_a_peer_shaped_hello...` |
| `local_terminal/authority.rs` `DirectPortAuthority::is_current_input_route` `(Some(_), true) => false` | `=> true` | `local_terminal_peer_socket::peer_hello_matches_the_offer_tuple...` |
| `scrollback_read.rs` `walk_page` (§2.8) | restore the old order (continue_read before the empty check) | `local_terminal_scrollback::post_read_authority_loss_suppresses_the_direct_scrollback_page` |
| `runtime/downstream/local_grants.rs` Ok arm | reply rpc-error | `link_downstream_local_grant::an_accepted_install_is_acknowledged_with_its_grant_id` |

## 6. Verification order for phase 2
1. Move drafts; apply §2 edits; `/tmp/wcheck.sh 'local_terminal|local_door|local_grants|owners.rs|link_ports|table.rs|retained_grid|scrollback_read|link_downstream'`.
2. `/tmp/wcargo.sh test -p roost-worker --test local_terminal_grants --test local_terminal_prehello --test local_terminal_socket --test local_terminal_socket_input --test local_terminal_peer_socket --test local_terminal_scrollback --test link_downstream_local_grant --test link_downstream_absent --test scrollback_read --test local_terminal_pty`
   plus any existing test that constructs `WorkerOwners::build` or `DownstreamOwners` (grep).
3. Mutations (§5). 4. `/tmp/wcargo.sh clippy -p roost-worker --all-targets -- -D warnings`.

## 7. Open questions / risks for the phase-2 agent
- Draft APIs not yet compiled: check `CellGridSnapshotPart::{Frame(PbCellGridFrame), Chunk(PbCellGridChunk)}`
  payload types, `PbCellGridChunk.part.as_option_mut()`, `EnumValue::from(ScrollbackHistoryFloor)` (`.into()`),
  `ServerFrame::from(<msg>)` From impls, `CellEmitter::sinks().contains(id)`, `session::cell_sink::local_cell_sink_id`,
  `Harness::deliver` driving the cadence (the "receives cells" / "stalled" tests rely on the cadence emitting
  to local sinks after a `terminalView` is accepted — if cells never arrive, check the view owner seeded the
  stream via `request_snapshot` and that `Fixture` did not register a second `coord` sink).
- The stream harness registers its own `RecordingSink` with id `coord`; `CellCadence::spawn` registers the
  real `CoordinatorCellSink` also as `coord` (replace-by-id expected). If that conflicts, build the emitter
  without the harness's sink.
- Parity gaps to report (shared reader, not fixed here): the Rust page reader refuses `max_rows == 0`
  (`scrollback_read::Refusal::EmptyRequest`) where v2 answers an empty page (`browser-command-terminal.ts:100`);
  v2 reads direct history one row per slice with a yield + epoch/core/eviction revalidation between slices
  (`:114-131`), the Rust walk is synchronous per 250-row slice with a single epoch re-check at the end
  (`local_terminal/scrollback.rs`); the browser-command page does not clamp `start_row` to the retained floor
  (v2 `:101` does; the direct reader does).
- `tests/local_terminal_socket.rs` uses `start_paused` in one test together with the real cadence; if paused time
  stalls the cadence, move that test to `local_terminal_prehello.rs` style (owner-only) or drive time with `advance`.
