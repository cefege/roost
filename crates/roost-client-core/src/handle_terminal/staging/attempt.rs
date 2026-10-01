//! One staged attempt, from the carrier that opened it to the id it published.
//!
//! Split from the Sync rotation beside it because the two run in OPPOSITE
//! directions and are read at opposite ends of a route's life. This is the
//! forward one: a carrier appears, the panes this document is viewing are
//! snapshotted, and the host is asked for an id per pane. The rotation is
//! backward: a route is lost, and the panes it was serving have to be
//! re-registered somewhere the coordinator will accept.
//!
//! Every rule here is a fence on ONE attempt. The attempt id, the pane's
//! snapshotted intent and revision, the connection's admissibility, and the id
//! itself all have to line up, because each one on its own is a way to publish
//! a second view for a pane that already has one.

use crate::effect::{DirectCommand, Effect, ViewIdTarget};
use crate::store::Store;
use crate::terminal::routes::{DirectCarrier, PromotionCandidate, ProspectiveView};
use crate::terminal::view::ViewIntent;
use crate::terminal::{TerminalSession, TerminalToken, TerminalView};

use super::{
    MintedViewId, canonical_still_matches, is_mintable_view_id, release_cancelled,
    wire_id_is_live_elsewhere,
};

/// How long a staged attempt has to earn its baseline.
///
/// v2's candidate deadline, unchanged: a candidate that never answers has not
/// failed in a way a longer wait would reveal, and the canonical route is still
/// painting behind it the whole time. The cost of waiting is a second view held
/// by the worker for five seconds; the cost of not waiting is a route elected on
/// a grid that stopped arriving.
const CANDIDATE_BASELINE_DEADLINE_MS: u64 = 5_000;

/// A carrier exists: for every session its grant admits and this document is
/// viewing, begin an attempt and ask the host for an id per pane.
///
/// This is the entry point the election was missing. `RouteRegistry::stage` used
/// to be reachable only from `fold_into_candidate`, which runs on the first
/// direct FRAME — but a frame only arrives for a socket the worker has been told
/// it is watching, the only thing that tells it is the view command published
/// here, and a view command needs a fresh id the worker has never seen. So
/// staging ends in a mint request, and the attempt begins as metadata promising
/// a view rather than as a live socket the worker refuses as a duplicate.
pub fn stage_viewed_sessions(
    store: &mut Store,
    carrier: &DirectCarrier,
    now_ms: u64,
    out: &mut Vec<Effect>,
) {
    let watched: Vec<String> = store
        .terminal
        .iter()
        .filter(|(session_id, _)| carrier.allows_session(session_id.as_str()))
        .filter(|(_, replica)| publishable_views(replica).next().is_some())
        .map(|(session_id, _)| session_id.clone())
        .collect();
    for session_id in watched {
        stage_one(store, &session_id, carrier, now_ms, out);
    }
}

/// Abandon a staged attempt, and tell its worker to release what it published.
///
/// The release is an `Unpublish` on the candidate's OWN token at one past the
/// revision it published under, because the worker reads a revision it has
/// already seen as a replay of the intent it is holding. A view with no minted id
/// is absent from `minted`: nothing was published, so there is nothing to
/// release, and inventing an id to unpublish would be a second id for the worker
/// to learn about.
pub fn cancel_staged_candidate(
    store: &mut Store,
    session_id: &str,
    reason: &str,
    out: &mut Vec<Effect>,
) {
    let Some(cancelled) = store.routes.cancel_candidate(session_id) else {
        return;
    };
    store.note_change();
    tracing::info!(
        target: "route",
        session_id = %cancelled.session_id,
        views = cancelled.minted.len(),
        reason,
        "a staged direct candidate was abandoned; the canonical route keeps the session"
    );
    release_cancelled(std::slice::from_ref(&cancelled), out);
}

/// Abandon a staged attempt and immediately start a new one for the same views.
///
/// Used where the attempt died because this document's INTENT moved — a pane
/// opened, resized, hid or closed — so the next attempt snapshots what the panes
/// want now rather than what they wanted when the carrier arrived.
pub fn cancel_staged_candidate_and_restart(
    store: &mut Store,
    session_id: &str,
    now_ms: u64,
    out: &mut Vec<Effect>,
) {
    if store.routes.candidate(session_id).is_none() {
        return;
    }
    cancel_staged_candidate(store, session_id, "the view set changed", out);
    if let Some(carrier) = store.routes.admitted_carrier(session_id) {
        stage_one(store, session_id, &carrier, now_ms, out);
    }
}

/// Abandon every attempt whose baseline deadline has passed.
pub fn sweep_candidate_deadlines(store: &mut Store, now_ms: u64, out: &mut Vec<Effect>) {
    let expired: Vec<String> = store
        .routes
        .staged_attempts()
        .into_iter()
        .filter(|(_, _, staged_at_ms)| {
            now_ms.saturating_sub(*staged_at_ms) >= CANDIDATE_BASELINE_DEADLINE_MS
        })
        .map(|(session_id, _, _)| session_id)
        .collect();
    for session_id in expired {
        cancel_staged_candidate(
            store,
            &session_id,
            "no baseline arrived before the candidate deadline",
            out,
        );
    }
}

/// Record a minted id on a staged candidate, and publish that view on it.
pub(super) fn adopt_candidate_view_id(
    store: &mut Store,
    minted: MintedViewId<'_>,
    now_ms: u64,
    out: &mut Vec<Effect>,
) {
    let MintedViewId {
        session_id,
        attempt_id,
        logical_view_id,
        wire_view_id,
        ..
    } = minted;
    let Some((source_intent, source_revision, candidate_revision, token)) =
        prospective_snapshot(store, session_id, attempt_id, logical_view_id)
    else {
        return;
    };
    if !store.routes.candidate_is_admissible(session_id) {
        tracing::debug!(
            target: "route",
            session_id,
            attempt_id,
            view_id = logical_view_id,
            "the connection that asked for this id is no longer admissible"
        );
        return;
    }
    if !canonical_still_matches(
        store,
        session_id,
        logical_view_id,
        source_intent,
        source_revision,
    ) {
        return;
    }
    let Some(wire_view_id) = accepted_wire_id(
        store,
        session_id,
        attempt_id,
        logical_view_id,
        wire_view_id,
        out,
    ) else {
        return;
    };
    let published = store
        .routes
        .staged_replica_mut(session_id)
        .is_some_and(|replica| {
            replica.bind_generation(&token);
            replica.open_prospective_view(
                logical_view_id,
                wire_view_id.clone(),
                source_intent,
                candidate_revision,
                now_ms,
            );
            // Awaited on the SOCKET generation, not the domain one: a direct
            // view-state is stamped by the drain with the socket it arrived on,
            // and awaiting anything else would make every answer look stale.
            replica.mark_view_published(logical_view_id, token.socket_generation, now_ms);
            true
        });
    if !published {
        return;
    }
    store
        .routes
        .mark_view_wire_id(session_id, logical_view_id, wire_view_id.clone());
    store.note_change();
    tracing::info!(
        target: "route",
        session_id,
        attempt_id,
        view_id = logical_view_id,
        wire_view_id = %wire_view_id,
        "a candidate published its own view id and is waiting for the authority's answer"
    );
    out.push(Effect::SendDirect {
        token,
        command: DirectCommand::View {
            session_id: session_id.to_string(),
            view_id: wire_view_id,
            intent: source_intent,
            revision: candidate_revision,
        },
    });
}

/// The id a candidate may publish under, or `None` after abandoning the attempt.
///
/// Three refusals, and each is a different defect: no id at all means the host
/// has no entropy; an id of the wrong shape means the host is not minting UUIDs;
/// a collision means two live views would answer to one handle, and the worker
/// resolves that by refusing the second — which is the socket closing, not the
/// second view being ignored.
fn accepted_wire_id(
    store: &mut Store,
    session_id: &str,
    attempt_id: u64,
    logical_view_id: &str,
    wire_view_id: Option<&str>,
    out: &mut Vec<Effect>,
) -> Option<String> {
    let reason = match wire_view_id {
        None => "the host minted no view id",
        Some(id) if !is_mintable_view_id(id) => {
            "the host minted a view id the worker will not accept"
        }
        Some(id) if wire_id_is_live_elsewhere(store, id) => "the minted view id collides",
        Some(id) => return Some(id.to_string()),
    };
    tracing::warn!(
        target: "route",
        session_id,
        attempt_id,
        view_id = logical_view_id,
        "a candidate cannot publish a view; the attempt is abandoned and Sync keeps the pane"
    );
    cancel_staged_candidate(store, session_id, reason, out);
    None
}

/// The panes of one session a candidate may publish: the ones still publishing. A
/// parked pane constrains no geometry and an unpublishing one is leaving, so
/// neither earns a second lease.
fn publishable_views(replica: &TerminalSession) -> impl Iterator<Item = &TerminalView> {
    replica
        .views()
        .values()
        .filter(|view| matches!(view.intent, ViewIntent::Publish { .. }))
}

/// Begin one attempt for one session.
fn stage_one(
    store: &mut Store,
    session_id: &str,
    carrier: &DirectCarrier,
    now_ms: u64,
    out: &mut Vec<Effect>,
) {
    if store.routes.candidate(session_id).is_some() {
        return;
    }
    let snapshots: Vec<(String, ViewIntent, u64)> = store
        .terminal(session_id)
        .map(|replica| {
            publishable_views(replica)
                .map(|view| (view.view_id.clone(), view.intent, view.revision))
                .collect()
        })
        .unwrap_or_default();
    if snapshots.is_empty() {
        return;
    }
    let attempt_id = store.next_attempt_id;
    store.next_attempt_id += 1;
    let prospective_views = snapshots
        .iter()
        .map(|(view_id, intent, revision)| {
            (
                view_id.clone(),
                ProspectiveView {
                    wire_view_id: None,
                    source_intent: *intent,
                    source_revision: *revision,
                    candidate_revision: revision + 1,
                    acknowledged: false,
                },
            )
        })
        .collect();
    let staged = store.routes.stage(
        PromotionCandidate {
            session_id: session_id.to_string(),
            connection_id: carrier.connection_id.clone(),
            token: carrier.token.clone(),
            attempt_id,
            baseline_ready: false,
            prospective_views,
            staged_at_ms: now_ms,
        },
        TerminalSession::new(session_id, &carrier.worker_fp),
    );
    if !staged {
        return;
    }
    store.note_change();
    tracing::info!(
        target: "route",
        session_id,
        connection_id = %carrier.connection_id,
        transport = carrier.transport.as_str(),
        views = snapshots.len(),
        attempt_id,
        "a direct carrier is staging a session this document is viewing"
    );
    for (logical_view_id, _, _) in snapshots {
        out.push(Effect::MintTerminalViewId {
            session_id: session_id.to_string(),
            attempt_id,
            logical_view_id,
            target: ViewIdTarget::Candidate,
        });
    }
}

/// The attempt's own snapshot of one pane, when that attempt is still the one
/// asking and has not already been answered.
fn prospective_snapshot(
    store: &Store,
    session_id: &str,
    attempt_id: u64,
    logical_view_id: &str,
) -> Option<(ViewIntent, u64, u64, TerminalToken)> {
    let candidate = store.routes.candidate(session_id)?;
    if candidate.attempt_id != attempt_id {
        tracing::debug!(
            target: "route",
            session_id,
            attempt_id,
            view_id = logical_view_id,
            "a view id answered an attempt that has moved on"
        );
        return None;
    }
    let prospective = candidate.prospective_views.get(logical_view_id)?;
    if prospective.wire_view_id.is_some() {
        tracing::debug!(
            target: "route",
            session_id,
            attempt_id,
            view_id = logical_view_id,
            "a view id answered an attempt that already has one"
        );
        return None;
    }
    Some((
        prospective.source_intent,
        prospective.source_revision,
        prospective.candidate_revision,
        candidate.token.clone(),
    ))
}
