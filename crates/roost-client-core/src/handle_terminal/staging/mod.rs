//! Moving a pane from one transport to another: staging a direct candidate, and
//! the Sync rotation that replaces ids when a direct route is lost.
//!
//! Both are the same problem in two directions. A pane's identity is the
//! renderer's, and never changes; the id the AUTHORITY holds is per transport,
//! and a worker refuses a second live socket on one. So every transport change
//! mints a fresh id per pane, keeps the old one alive until the new one is
//! proven, and only then retires the old one.
//!
//! Ported from `apps/web/src/store/transport/terminal-promotion-candidate.ts`.
//! Contract: `protocol/spec/direct-terminal.md`; the reasons are in
//! `docs/phase4-client-contract.md` §8.

use roost_protocol::viewport::is_terminal_uuid;

use crate::effect::{DirectCommand, Effect, ViewIdTarget};
use crate::store::Store;
use crate::terminal::routes::CancelledCandidate;
use crate::terminal::view::ViewIntent;

mod attempt;
mod rotation;

use attempt::adopt_candidate_view_id;
use rotation::adopt_sync_view_id;

pub use attempt::{
    cancel_staged_candidate, cancel_staged_candidate_and_restart, stage_admitted_sessions,
    stage_viewed_sessions, sweep_candidate_deadlines,
};
pub use rotation::begin_sync_view_rotation;

/// A host answer that is a view id the worker will accept.
///
/// `is_terminal_uuid` is the shape rule, shared with the coordinator and the
/// worker; the version is checked on top because the only source of these ids is
/// `crypto.randomUUID`, which is a v4. A host that answered with a time-based or
/// nil id would be publishing a handle the worker's own `is_v2_uuid` admits and
/// the coordinator's revision floor cannot reason about, so the core refuses it
/// here rather than learning that on a round trip.
fn is_mintable_view_id(candidate: &str) -> bool {
    is_terminal_uuid(candidate) && candidate.as_bytes()[14] == b'4'
}

/// The ids a cancelled attempt's worker is still holding, released on the token
/// it published them on.
pub fn release_cancelled(cancelled: &[CancelledCandidate], out: &mut Vec<Effect>) {
    for cancelled in cancelled {
        for (wire_view_id, revision) in &cancelled.minted {
            out.push(Effect::SendDirect {
                token: cancelled.token.clone(),
                command: DirectCommand::View {
                    session_id: cancelled.session_id.clone(),
                    view_id: wire_view_id.clone(),
                    intent: ViewIntent::Unpublish,
                    revision: revision + 1,
                },
            });
        }
    }
}

/// The host's answer to a mint request, as the core reads it.
///
/// A record rather than seven positional arguments, for the reason `ViewOpen` is
/// one: the target and the attempt id are BOTH fences, and a caller that swaps
/// them still compiles — and then acts on the wrong attempt, which is the one
/// mistake here that publishes a second id for a pane that already has one.
#[derive(Debug, Clone, Copy)]
pub struct MintedViewId<'a> {
    /// The session the pane belongs to.
    pub session_id: &'a str,
    /// The attempt the request named.
    pub attempt_id: u64,
    /// The pane's own identity, which never changes.
    pub logical_view_id: &'a str,
    /// Which attempt asked.
    pub target: ViewIdTarget,
    /// The minted id, or `None` when the host could not mint one.
    pub wire_view_id: Option<&'a str>,
}

/// A host answered a mint request.
///
/// Two attempts ask for ids and neither may take the other's answer: a candidate
/// is preparing ids for a carrier still on trial, and a rotation is replacing
/// ids whose carrier is already gone. The target is the first fence, the attempt
/// id the second, and after both the source view has to be exactly what the
/// attempt snapshotted — a pane that moved in the meantime invalidates the whole
/// snapshot, not just its own id.
pub fn handle_view_id_minted(
    store: &mut Store,
    minted: MintedViewId<'_>,
    now_ms: u64,
    out: &mut Vec<Effect>,
) {
    match minted.target {
        ViewIdTarget::Candidate => adopt_candidate_view_id(store, minted, now_ms, out),
        ViewIdTarget::SyncFallback => adopt_sync_view_id(store, minted, now_ms, out),
    }
}

/// Whether this document still holds the pane exactly as the attempt read it.
fn canonical_still_matches(
    store: &Store,
    session_id: &str,
    logical_view_id: &str,
    intent: ViewIntent,
    revision: u64,
) -> bool {
    let matches = store
        .terminal(session_id)
        .and_then(|replica| replica.view(logical_view_id))
        .is_some_and(|view| view.intent == intent && view.revision == revision);
    if !matches {
        tracing::debug!(
            target: "route",
            session_id,
            view_id = logical_view_id,
            "the pane changed while an id was being minted"
        );
    }
    matches
}

/// Whether any view this document holds is already published under this id.
fn wire_id_is_live_elsewhere(store: &Store, wire_view_id: &str) -> bool {
    store.terminal.iter().any(|(_, replica)| {
        replica
            .views()
            .values()
            .any(|view| view.wire_view_id == wire_view_id)
    })
}
