//! The Sync socket's life: dial, close, redial, the stale watchdog, lifecycle
//! wakes, and the transport controls.
//!
//! Called by `handle_event` for `DialRequested`, `SyncLinkClosed`,
//! `PageVisibilityChanged`, `SyncWakeRequested`, `SyncTransportControl` and
//! every `Sweep`. Ported from `apps/web/src/store/sync.ts` (`_runConnectSync`'s
//! dial/close loop), `apps/web/src/store/sync-redial.ts`,
//! `apps/web/src/store/sync-watchdog.ts` (`startStaleWatchdog`) and
//! `apps/web/src/store/sync-smoke.ts`.

use crate::client::sync::classify_close;
use crate::effect::Effect;
use crate::store::Store;
use crate::store::root::{BrowserAccessState, mark_browser_device_rejected};
use crate::sync::SYNC_AUTH_REVOKED_CLOSE_CODE;
use crate::sync::redial::{
    SYNC_STALE_TIMEOUT_MS, SyncLinkLiveness, should_close_stale_link_on_resume,
};
use crate::terminal::liveness::RepairOutcome;
use crate::terminal::token::TerminalTransport;

use super::hydration::{request_link_replacement, start_bootstrap_probe, sweep_hydrations};

/// A deliberate transport control.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportControl {
    /// Replace the live socket now (v2 `reconnectNow` / `forceSyncReconnect`).
    Reconnect,
    /// Drop the socket with the redial pre-armed to its highest floor
    /// (`forceSyncMaxBackoff`).
    ArmMaxBackoff,
    /// Close the socket and hold the redial (`pauseSyncTransport`).
    Pause,
    /// Release the hold and redial now (`resumeSyncTransport`).
    Resume,
}

/// Open a Sync socket, unless the credential was refused.
pub(crate) fn dial(store: &mut Store, now_ms: u64, out: &mut Vec<Effect>) {
    if store.sync.auth_revoked {
        // Dialing again would present the same rejected credential.
        tracing::warn!(target: "sync", "dial declined: the credential was revoked");
        return;
    }
    store.sync.redial.note_dial_started();
    let (generation, dial) = store.sync.begin_dial(&store.tab_id);
    store.sync.hydrations.note_dial_started(now_ms);
    store.note_change();
    tracing::info!(target: "sync", generation, "sync dial");
    out.push(Effect::DialSync { generation, dial });
}

/// The socket of `generation` closed, opened or not.
pub(crate) fn on_link_closed(
    store: &mut Store,
    generation: u64,
    close_code: Option<u16>,
    close_reason: &str,
    now_ms: u64,
) {
    let was_open = store.sync.close_link(generation, close_code);
    let latest = generation == store.sync.dial_count();
    // A socket refused before `subscribed` still carries the coordinator's
    // verdict: 4001 is a revoked key whether or not the link ever opened.
    if !was_open && latest && close_code == Some(SYNC_AUTH_REVOKED_CLOSE_CODE) {
        store.sync.auth_revoked = true;
    }
    if !was_open && !latest {
        return;
    }
    store.note_change();
    store.sync.hydrations.clear_for_new_socket();
    release_sync_generation(store, generation);
    if store.sync.auth_revoked {
        tracing::error!(target: "sync", generation, "sync credential revoked; not redialing");
        mark_browser_device_rejected(store, "sync_4001");
        return;
    }
    // A backpressure 1013 held the records rather than dropping them, so the
    // redial is immediate and resumes from the cursor (v2 `flow`); the SAME code
    // with the rejection reason gets the backoff.
    let disposition = classify_close(close_code, close_reason);
    if let Some(reason) = disposition.abort_reason() {
        store.sync.redial.note_close_abort_reason(reason.as_str());
    }
    store.sync.redial.schedule_after_close(now_ms);
    let status = store.sync.redial.status();
    tracing::info!(
        target: "sync",
        generation,
        close_code,
        disposition = disposition.as_str(),
        failures = status.failures,
        delay_ms = status.next_delay_ms,
        parked = status.hidden_parked,
        "sync link closed; redial scheduled"
    );
}

/// Unbind every replica fenced to the Sync socket that just closed, settle
/// every batch that socket was carrying, and block every input lane whose route
/// the worker acknowledged over it.
///
/// v2 retargets each session to the current Sync state when the socket goes,
/// and with no state that token is null: the grid stays painted but no carrier
/// is confirmed for it, so the header reads Waiting until the next link binds it
/// (`terminal-stream-retarget.ts` `retargetSession`). A replica on a direct
/// route holds the direct token and is untouched. A batch the closed socket
/// carried can never be answered on another one, so it settles now
/// (`sync-outbound.ts` `handleGeneration`, "Sync closed"). A route epoch the
/// closed socket claimed fences nothing on the next one, so its lane is blocked
/// until Sync claims the route back (`retireTerminalInputRouteState`).
fn release_sync_generation(store: &mut Store, generation: u64) {
    for outcome in store
        .input
        .retire_sync_generation(generation, "Sync closed")
    {
        tracing::warn!(
            target: "terminal",
            generation,
            input_seq = outcome.input_seq(),
            status = outcome.status_name(),
            "terminal input settled by Sync close"
        );
    }
    for session_id in store.input.block_routes_on_sync_socket(generation) {
        tracing::info!(
            target: "route",
            session_id,
            generation,
            "the closed Sync socket held this session's input route; it is claimed back on the next keystroke"
        );
    }
    let mut released = 0_usize;
    for replica in store.terminal.values_mut() {
        let fenced_here = replica.generation().is_some_and(|token| {
            token.transport == TerminalTransport::Sync && token.socket_generation == generation
        });
        if fenced_here {
            let _ = replica.release_generation();
            replica.retire_liveness(RepairOutcome::GenerationReset);
            released += 1;
        }
    }
    if released > 0 {
        tracing::info!(
            target: "terminal",
            generation,
            released,
            "sync link closed; the replicas fenced to it confirm no carrier"
        );
    }
}

/// The per-sweep Sync duties: the due redial, the stale watchdog, hydration
/// deadlines and retries, and the bootstrap probe.
pub(crate) fn sweep_sync(store: &mut Store, now_ms: u64, out: &mut Vec<Effect>) {
    if store.sync.redial.take_due(now_ms) {
        dial(store, now_ms, out);
    }
    if let Some(link) = &store.sync.link {
        let idle_ms = now_ms.saturating_sub(link.last_frame_at_ms);
        if link.open
            && link.accepting
            && store.sync.redial.visible()
            && idle_ms >= SYNC_STALE_TIMEOUT_MS
        {
            tracing::warn!(target: "sync", idle_ms, "sync link stale; replacing it");
            request_link_replacement(store, "stale", out);
        }
    }
    sweep_hydrations(store, now_ms, out);
    if store.browser_access_state == BrowserAccessState::Checking
        && store.sync.hydrations.take_probe_due(now_ms)
    {
        start_bootstrap_probe(store, out);
    }
}

/// The document's visibility changed. A hidden document stops pre-warming, and
/// one that shows again is pre-warmed afresh.
pub(crate) fn on_visibility(store: &mut Store, visible: bool, now_ms: u64, out: &mut Vec<Effect>) {
    store.sync.redial.set_visible(visible);
    tracing::debug!(target: "sync", visible, "page visibility");
    if visible {
        on_wake(store, false, now_ms, out);
    }
    crate::handle_terminal::reconcile_prewarm(store, now_ms, out);
}

/// A page-lifecycle wake: resume the redial now, and replace an open socket
/// that went silent past the refocus budget.
pub(crate) fn on_wake(store: &mut Store, allow_hidden: bool, now_ms: u64, out: &mut Vec<Effect>) {
    if store
        .sync
        .redial
        .take_lifecycle_wake(now_ms, allow_hidden)
        .is_none()
    {
        return;
    }
    tracing::info!(target: "sync", allow_hidden, "sync lifecycle wake");
    if store.sync.redial.visible()
        && should_close_stale_link_on_resume(liveness(store), idle_ms(store, now_ms))
    {
        request_link_replacement(store, "visibility", out);
    }
}

/// Apply one transport control.
pub(crate) fn on_transport_control(
    store: &mut Store,
    control: TransportControl,
    now_ms: u64,
    out: &mut Vec<Effect>,
) {
    tracing::info!(target: "sync", ?control, "sync transport control");
    match control {
        TransportControl::Reconnect => {
            store.sync.redial.resume_now(now_ms);
            request_link_replacement(store, "manual", out);
        }
        TransportControl::ArmMaxBackoff => {
            store.sync.redial.arm_floor();
            // Dropped the way a network failure drops it: no immediate reason.
            request_link_replacement(store, "network", out);
        }
        TransportControl::Pause => {
            store.sync.redial.set_smoke_paused(true);
            request_link_replacement(store, "manual", out);
        }
        TransportControl::Resume => {
            store.sync.redial.set_smoke_paused(false);
            store.sync.redial.resume_now(now_ms);
        }
    }
}

/// Whether there is a socket, and whether it carries traffic.
pub fn liveness(store: &Store) -> SyncLinkLiveness {
    match &store.sync.link {
        Some(link) if link.open => SyncLinkLiveness::Open,
        Some(_) => SyncLinkLiveness::Dialing,
        None if store.sync.redial.is_pending() => SyncLinkLiveness::None,
        None if store.sync.dial_count() > 0 => SyncLinkLiveness::Dialing,
        None => SyncLinkLiveness::None,
    }
}

fn idle_ms(store: &Store, now_ms: u64) -> u64 {
    store
        .sync
        .link
        .as_ref()
        .map_or(0, |link| now_ms.saturating_sub(link.last_frame_at_ms))
}
