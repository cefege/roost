//! The four mutations that can settle — or retract — an attempt.
//!
//! Split from the ledger because the ledger is STATE and these are the RULES over
//! it, and the rules are where the superseded answer is decided. Every one of
//! them takes a [`SpawnTicket`] and returns a [`SpawnSettlement`], and none of
//! them asks the caller whether it is still relevant.

use roost_protocol::viewport::is_terminal_uuid;

use super::{
    ClientOnlySession, EntryState, SpawnEntry, SpawnRefusal, SpawnSettlement, SpawnTicket,
    SupersededReason, TombstoneReason,
};
use crate::store::Store;
use crate::store::toasts::{ToastId, ToastKind, ToastOptions, ToastSource, raise_toast};

/// Begin an optimistic spawn: mint the attempt, hold the placeholder, and hand
/// the caller the ticket every later answer must quote.
///
/// `session_id` is the host's UUID. The placeholder takes the anchor's machine
/// and folder, so the pane it opens is already the right size and the right
/// place, and `created_at_ms` is the host's single reading of the clock — the
/// same instant the sidebar's ordering and every deadline in this crate are
/// measured against.
pub fn begin_optimistic_spawn(
    store: &mut Store,
    session_id: impl Into<String>,
    anchor_worker_fp: impl Into<String>,
    anchor_cwd: impl Into<String>,
    anchor_workspace_id: Option<String>,
    now_ms: u64,
) -> Result<SpawnTicket, SpawnRefusal> {
    let session_id = session_id.into();
    if !is_terminal_uuid(&session_id) {
        return Err(SpawnRefusal::NotAUuid { session_id });
    }
    let attempt = store.spawns.mint_attempt();
    let ticket = SpawnTicket {
        session_id: session_id.clone(),
        attempt,
    };
    let cwd = anchor_cwd.into();
    store.spawns.entries.insert(
        session_id.clone(),
        SpawnEntry {
            ticket: ticket.clone(),
            placeholder: ClientOnlySession {
                id: session_id.clone(),
                worker_fp: anchor_worker_fp.into(),
                cwd: cwd.clone(),
                spawn_cwd: cwd,
                workspace_id: anchor_workspace_id,
                created_at_ms: i64::try_from(now_ms).unwrap_or(i64::MAX),
            },
            state: EntryState::Pending,
        },
    );
    store.note_change();
    tracing::debug!(target: "store", session_id, attempt, "optimistic spawn began");
    Ok(ticket)
}

/// Begin a NEWER attempt for a session this browser is already spawning.
///
/// This is the superseded case the ledger exists for: the newer attempt takes
/// over the session id and its placeholder, and the older attempt's late answer
/// is refused rather than rolled back.
pub fn respawn_optimistic_spawn(
    store: &mut Store,
    ticket: &SpawnTicket,
    now_ms: u64,
) -> Result<SpawnTicket, SpawnRefusal> {
    let attempt = store.spawns.mint_attempt();
    let next = SpawnTicket {
        session_id: ticket.session_id.clone(),
        attempt,
    };
    let Some(entry) = store.spawns.entries.get_mut(&ticket.session_id) else {
        return Err(SpawnRefusal::NoPendingSpawn {
            session_id: ticket.session_id.clone(),
        });
    };
    entry.ticket = next.clone();
    entry.placeholder.created_at_ms = i64::try_from(now_ms).unwrap_or(i64::MAX);
    entry.state = EntryState::Pending;
    store.note_change();
    tracing::debug!(target: "store", session_id = %next.session_id, attempt, "optimistic spawn respawned");
    Ok(next)
}

/// The coordinator admitted the spawn.
///
/// The placeholder is NOT removed: it is the tab the user is looking at until
/// the authoritative row replaces it. What the admission releases is the pending
/// state — a late rejection arriving after this is `AlreadySettled` and changes
/// nothing, which is the hole `optimisticSpawn.ts:187-197` leaves open.
pub fn settle_spawn_admitted(store: &mut Store, ticket: &SpawnTicket) -> SpawnSettlement {
    if let Some(reason) = store.spawns.relevance(ticket) {
        return refuse(ticket, reason, "admitted");
    }
    if let Some(entry) = store.spawns.entries.get_mut(&ticket.session_id) {
        entry.state = EntryState::Admitted;
    }
    let session_id = ticket.session_id.clone();
    let attempt = ticket.attempt;
    store
        .spawns
        .remember(session_id, attempt, TombstoneReason::Admitted);
    store.note_change();
    tracing::info!(target: "store", session_id = %ticket.session_id, attempt, "optimistic spawn admitted");
    SpawnSettlement::Applied
}

/// The coordinator refused the spawn: drop the placeholder and say so.
///
/// A REFUSAL FOR A SUPERSEDED ATTEMPT IS SILENT. It does not remove the newer
/// attempt's placeholder, and it raises no card — a failure the user already
/// caused by closing the tab, or that a newer request has already superseded, is
/// not a failure they are waiting to hear about.
pub fn settle_spawn_rejected(
    store: &mut Store,
    ticket: &SpawnTicket,
    message: &str,
    now_ms: u64,
) -> SpawnSettlement {
    if let Some(reason) = store.spawns.relevance(ticket) {
        return refuse(ticket, reason, "rejected");
    }
    let session_id = ticket.session_id.clone();
    let attempt = ticket.attempt;
    store.spawns.entries.remove(&session_id);
    store
        .spawns
        .remember(session_id.clone(), attempt, TombstoneReason::Rejected);
    // ONE bump for two writes: dropping the placeholder and raising the card are
    // one user-visible event, and two bumps would be two repaints of it.
    raise_toast(
        &mut store.toasts,
        ToastId::new(ToastSource::Spawn { attempt }, session_id.clone()),
        format!("New terminal failed: {message}"),
        ToastKind::Err,
        ToastOptions::plain(),
        now_ms,
    );
    store.note_change();
    tracing::warn!(
        target: "store",
        session_id = %session_id,
        attempt,
        message,
        "optimistic spawn rejected"
    );
    SpawnSettlement::Applied
}

/// The user closed the pending tab before the answer landed.
///
/// Nothing is raised and nothing is logged as an error: the caller reaps the real
/// PTY once the in-flight spawn lands, and the answer that follows is the
/// expected removal this tombstone names.
pub fn abort_optimistic_spawn(store: &mut Store, ticket: &SpawnTicket) -> bool {
    if store.spawns.relevance(ticket).is_some() {
        return false;
    }
    let session_id = ticket.session_id.clone();
    let attempt = ticket.attempt;
    store.spawns.entries.remove(&session_id);
    store
        .spawns
        .remember(session_id.clone(), attempt, TombstoneReason::Retracted);
    store.note_change();
    tracing::info!(target: "store", session_id, attempt, "optimistic spawn retracted");
    true
}

/// The authoritative row for `session_id` arrived; the placeholder has done its
/// job.
///
/// Called when the session plane gains the id. Without it the tab would sit
/// client-only over a session that already exists, and every membership
/// projection would keep excluding a live row.
pub fn reconcile_spawn(store: &mut Store, session_id: &str) -> bool {
    if store.spawns.entries.remove(session_id).is_none() {
        return false;
    }
    store.note_change();
    true
}

/// The one place a refused settlement is recorded, so the reason and the refusal
/// cannot be reported from two places.
fn refuse(ticket: &SpawnTicket, reason: SupersededReason, answer: &str) -> SpawnSettlement {
    tracing::debug!(
        target: "store",
        session_id = %ticket.session_id,
        attempt = ticket.attempt,
        answer,
        ?reason,
        "ignored a superseded spawn answer"
    );
    SpawnSettlement::Superseded(reason)
}
