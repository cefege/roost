//! The smoke backdoor's reach into the Sync transport: the partition controls
//! (each one `ClientEvent::SyncTransportControl`), the redial report, the ready
//! terminal generation, and the frame faults fenced to it. Called only by
//! roost-web's `smoke::backdoor`. Ports `apps/web/src/store/sync-smoke.ts`, the
//! status read of `apps/web/src/store/sync-redial.ts:26-35`, and the arm half of
//! `apps/web/src/store/terminal-stream-diagnostics.ts:196-225`.

pub use crate::handle_sync::lifecycle::TransportControl;

use crate::store::Store;
use crate::sync::SyncDomain;
use crate::sync::redial::SyncLinkLiveness;
use crate::terminal::token::{TerminalToken, TerminalTransport};

/// What `syncRedialStatus()` reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyncRedialReport {
    /// Consecutive failed dials since this tab last received a Sync frame.
    pub failures: u32,
    /// The delay the pending redial waits — capped, never unbounded.
    pub next_delay_ms: u64,
    /// True only while a hidden document sleeps instead of redialing.
    pub hidden_parked: bool,
    /// Whether there is an open socket, a dial in flight, or neither.
    pub liveness: SyncLinkLiveness,
}

/// The redial loop's state and the socket's liveness, read together.
pub fn sync_redial_report(store: &Store) -> SyncRedialReport {
    let status = store.sync.redial.status();
    SyncRedialReport {
        failures: status.failures,
        next_delay_ms: status.next_delay_ms,
        hidden_parked: status.hidden_parked,
        liveness: crate::handle_sync::lifecycle::liveness(store),
    }
}

/// The live socket's terminal generation, once its terminal domain is ready
/// (v2 `currentSyncV2TerminalState()` with `ready`). A smoke fault armed
/// against anything else would be fenced to a generation that is not serving.
pub fn ready_terminal_generation(store: &Store) -> Option<TerminalToken> {
    if !store.sync.domain_is_ready(SyncDomain::Terminal) {
        return None;
    }
    store.sync_terminal_token()
}

/// The generation a session's frames are owned by right now, for arming a
/// fault against (v2 `currentTerminalSmokeGeneration`): the replica's bound
/// token, and for a Sync-bound replica only while the terminal domain is ready.
pub fn session_fault_generation(store: &Store, session_id: &str) -> Option<TerminalToken> {
    let token = store.terminal(session_id)?.generation()?.clone();
    if token.transport == TerminalTransport::Sync && ready_terminal_generation(store).is_none() {
        return None;
    }
    Some(token)
}

/// Drop every Sync frame for `session_id` until its generation moves. `false`
/// when the session has no generation to fence the fault to, which arms
/// nothing (v2 returns without arming in the same case).
pub fn arm_terminal_blackhole(store: &mut Store, session_id: &str) -> bool {
    let Some(generation) = session_fault_generation(store, session_id) else {
        return false;
    };
    store
        .terminal_smoke_faults
        .arm_blackhole(session_id, generation);
    true
}

/// Drop exactly the next non-full Sync frame for `session_id` after it is
/// acknowledged. `false` when there is no generation to fence it to.
pub fn arm_terminal_wire_delta_drop(store: &mut Store, session_id: &str) -> bool {
    let Some(generation) = session_fault_generation(store, session_id) else {
        return false;
    };
    store
        .terminal_smoke_faults
        .arm_wire_delta_drop(session_id, generation);
    true
}
