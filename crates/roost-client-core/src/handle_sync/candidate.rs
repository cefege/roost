//! The direct candidate: where its frames go, and the one commit that makes it
//! the session's route.
//!
//! Every rule here is about the ORDER of three things — the view command that
//! makes the worker stream, the stream's identity, and the grid — because a
//! candidate that gets any of them early paints a grid that belongs to a
//! different question than the one the reader is asking. The order is:
//!
//! 1. the host mints a wire id the worker has never seen, and the candidate
//!    publishes that id (in `handle_terminal::staging`);
//! 2. the worker answers with a `ViewState` naming that id and the stream it is
//!    now minting, and only then does the staged replica hold an expectation;
//! 3. a complete full arrives for THAT stream, and only then may the candidate
//!    be promoted.
//!
//! Step 2 is not optional. `frame_fold::valid_full` refuses a frame with no
//! expected stream, so without the acknowledgement the candidate can never hold
//! a baseline, `promote` never fires, and the cycle does not close.
//!
//! Contract: `protocol/spec/direct-terminal.md`; the reasons are in
//! `docs/phase4-client-contract.md` §8.

use crate::client::carriers::SignallingInput;
use crate::effect::{DirectCommand, Effect, SyncCommand};
use crate::handle_sweep::request_candidate_repair_if_due;
use crate::store::Store;
use crate::sync::SyncFrame;
use crate::terminal::session::{Admission, TerminalSession, ViewStateAdmission};
use crate::terminal::token::{TerminalToken, TerminalTransport};
use crate::terminal::view::{ViewIntent, ViewStateResult};
use roost_protocol::viewport::{TerminalGeometry, is_terminal_geometry, is_terminal_uuid};

/// Fold one direct-carrier cell frame into the session's staged replica.
///
/// The replica is folded IN PLACE. A `TerminalSession` owns a chunk assembler and
/// cannot be cloned, so it is created once when the attempt is staged and
/// borrowed mutably by every frame after — which is also the only way a candidate
/// can accumulate a baseline instead of restarting it per frame.
pub(super) fn fold_into_candidate<F>(
    store: &mut Store,
    session_id: &str,
    token: &TerminalToken,
    now_ms: u64,
    out: &mut Vec<Effect>,
    fold: F,
) where
    F: FnOnce(&mut TerminalSession) -> Admission,
{
    let Some((attempt_id, attempt_token)) = fenced_attempt(store, session_id, token) else {
        return;
    };
    let (painted_before, painted_after, baseline_ready, admission) = {
        let Some(replica) = store.routes.staged_replica_mut(session_id) else {
            return;
        };
        replica.bind_generation(token);
        let before = replica.frame_revision();
        let admission = fold(replica);
        (
            before,
            replica.frame_revision(),
            replica.baseline_ready(),
            admission,
        )
    };
    store
        .routes
        .mark_candidate_baseline(session_id, baseline_ready);
    if painted_after != painted_before {
        store.note_change();
    }
    // Before the baseline check: a candidate whose first delta is refused asks
    // at once rather than at the next sweep.
    if matches!(admission, Admission::Refused { latched: true, .. }) {
        request_candidate_repair_if_due(store, session_id, now_ms, out);
    }
    if !baseline_ready || !every_view_acknowledged(store, session_id) {
        return;
    }
    // PROMOTE, which is what makes a candidate the elected route. Without this
    // step a carrier can authenticate, stage a replica and paint a baseline,
    // and `snapshot.route.active` still reads `sync` forever — the terminal
    // stays on the fallback while a working direct path sits beside it
    // unelected. The promotion first claims the worker's input route for the
    // candidate, and `promote` is the fence at the commit: it refuses a
    // candidate whose attempt moved on, whose baseline is incomplete, whose
    // views are unanswered, whose token changed, or whose connection is gone,
    // and every one of those refusals is a reason NOT to switch.
    super::promotion::advance_promotion(store, session_id, attempt_id, &attempt_token, now_ms, out);
}

/// Apply a view-state answer that arrived on a direct carrier.
///
/// It goes to the CANDIDATE's own view, matched by the id the candidate minted,
/// and its generation is the socket's — the drain stamped it, and a result
/// correlated on anything else would read as an answer to a socket that is gone.
pub(super) fn apply_direct_view_state(
    store: &mut Store,
    session_id: &str,
    token: &TerminalToken,
    frame: &SyncFrame,
    now_ms: u64,
    out: &mut Vec<Effect>,
) {
    let SyncFrame::ViewState {
        view_id,
        generation,
        revision,
        accepted,
        stream_id,
        effective_cols,
        effective_rows,
        ..
    } = frame
    else {
        return;
    };
    if fenced_attempt(store, session_id, token).is_none() {
        return;
    }
    let Some(logical_view_id) = logical_view_for_wire(store, session_id, view_id) else {
        tracing::debug!(
            target: "route",
            session_id,
            view_id,
            "a direct view-state named an id this candidate never published"
        );
        return;
    };
    let result = ViewStateResult {
        session_id: session_id.to_string(),
        view_id: logical_view_id.clone(),
        generation: *generation,
        revision: *revision,
        accepted: *accepted,
        stream_id: (!stream_id.is_empty()).then(|| stream_id.clone()),
        effective_cols: *effective_cols,
        effective_rows: *effective_rows,
    };
    let admission = store
        .routes
        .staged_replica_mut(session_id)
        .map(|replica| replica.apply_view_state(&result, now_ms));
    match admission {
        Some(ViewStateAdmission::Stale) => tracing::debug!(
            target: "route",
            session_id,
            view_id = logical_view_id,
            "a direct view-state answered a socket this candidate is not folded on"
        ),
        Some(ViewStateAdmission::Refused) => {
            tracing::warn!(
                target: "route",
                session_id,
                view_id = logical_view_id,
                "the worker refused the candidate's own view; the attempt is abandoned"
            );
            crate::handle_terminal::cancel_staged_candidate(
                store,
                session_id,
                "the authority refused the candidate's view",
                out,
            );
        }
        Some(ViewStateAdmission::Accepted { stream_id }) => {
            if !install_candidate_stream(
                store,
                session_id,
                stream_id.as_deref(),
                *effective_cols,
                *effective_rows,
                out,
            ) {
                return;
            }
            store
                .routes
                .mark_view_acknowledged(session_id, &logical_view_id);
            request_candidate_repair_if_due(store, session_id, now_ms, out);
        }
        None => {}
    }
}

/// Install the stream an accepted answer named, or abandon the attempt.
///
/// Returns false when the attempt is over. An accepted answer with no stream
/// installs nothing — the same rule the canonical path follows, and for the same
/// reason: a replica expecting no stream admits no full, so a silent success
/// here would be a candidate that can never be promoted and never says why.
fn install_candidate_stream(
    store: &mut Store,
    session_id: &str,
    stream_id: Option<&str>,
    cols: u32,
    rows: u32,
    out: &mut Vec<Effect>,
) -> bool {
    let Some(stream_id) = stream_id else {
        return true;
    };
    let geometry = TerminalGeometry { cols, rows };
    if !is_terminal_uuid(stream_id) || !is_terminal_geometry(&geometry) {
        tracing::warn!(
            target: "route",
            session_id,
            "the candidate's view was accepted without a usable stream; the attempt is abandoned"
        );
        crate::handle_terminal::cancel_staged_candidate(
            store,
            session_id,
            "the accepted view named no usable stream",
            out,
        );
        return false;
    }
    let Some(replica) = store.routes.staged_replica_mut(session_id) else {
        return false;
    };
    if replica.install_expected_stream(stream_id, cols, rows) {
        store.note_change();
        tracing::info!(
            target: "route",
            session_id,
            stream_id,
            "the candidate expects a fresh baseline"
        );
    }
    true
}

/// The attempt a frame on `token` may touch, when that frame's token and
/// connection are still the ones the attempt was prepared on.
fn fenced_attempt(
    store: &Store,
    session_id: &str,
    token: &TerminalToken,
) -> Option<(u64, TerminalToken)> {
    let candidate = store.routes.candidate(session_id)?;
    if &candidate.token != token {
        tracing::debug!(
            target: "route",
            session_id,
            "a direct frame arrived on a generation the attempt is not folded on"
        );
        return None;
    }
    if !store.routes.candidate_is_admissible(session_id) {
        tracing::debug!(
            target: "route",
            session_id,
            "a direct frame arrived on a connection the registry no longer holds"
        );
        return None;
    }
    Some((candidate.attempt_id, candidate.token.clone()))
}

/// Whether every view the candidate published has been answered.
fn every_view_acknowledged(store: &Store, session_id: &str) -> bool {
    store.routes.candidate(session_id).is_some_and(|candidate| {
        !candidate.prospective_views.is_empty()
            && candidate
                .prospective_views
                .values()
                .all(|view| view.acknowledged)
    })
}

/// The pane a candidate's minted id belongs to.
fn logical_view_for_wire(store: &Store, session_id: &str, wire_view_id: &str) -> Option<String> {
    store.routes.candidate(session_id).and_then(|candidate| {
        candidate
            .prospective_views
            .iter()
            .find(|(_, prospective)| prospective.wire_view_id.as_deref() == Some(wire_view_id))
            .map(|(logical_view_id, _)| logical_view_id.clone())
    })
}

/// Swap the promoted replica in, and only THEN retire what it replaced.
///
/// The ordering is the whole safety argument, and it is v2's
/// (`TerminalPromotionCandidate`): the candidate publishes NEW ids without
/// touching Sync, and on commit it adopts them before the old ids are retired on
/// the old transport. Reversed, a worker that dropped the old lease between the
/// two would have no view at all — and the reader would be left with painted rows
/// and a stream nothing is feeding.
pub(super) fn commit_promotion(
    store: &mut Store,
    session_id: &str,
    mut promoted: TerminalSession,
    token: &TerminalToken,
    now_ms: u64,
    out: &mut Vec<Effect>,
) {
    // Read BEFORE the replica is replaced: these are the identities the previous
    // route was serving, and they are gone the moment `insert` runs.
    let previous = store.terminal(session_id).map(|replica| {
        (
            replica.generation().cloned(),
            replica
                .views()
                .values()
                .filter(|view| view.intent != ViewIntent::Unpublish)
                .map(|view| (view.wire_view_id.clone(), view.revision))
                .collect::<Vec<_>>(),
        )
    });
    promoted.inherit_wire_record(store.terminal(session_id));
    store.terminal.insert(session_id.to_string(), promoted);
    store.note_change();
    tracing::info!(
        target: "route",
        session_id,
        transport = token.transport.as_str(),
        "a direct candidate earned its baseline and is now the elected route"
    );
    if let Some((Some(old_token), old_views)) = previous {
        release_previous(store, session_id, &old_token, &old_views, out);
    }
    // The machine learns a peer is ELECTED, which is a fact about its attempt and
    // not about the route table: without it a negotiated carrier stays
    // `Candidate` for ever and the snapshot reports a fault state over a working
    // route.
    if token.transport == TerminalTransport::Peer
        && let Some(worker_fp) = token.worker_fp.clone()
    {
        store.direct.transport_observed(
            &worker_fp,
            SignallingInput::PromotionCommitted {
                session_id: session_id.to_owned(),
                token: token.clone(),
                now_ms,
            },
            now_ms,
            out,
        );
    }
}

/// Retire the ids the previous route was serving, on the route that served them.
///
/// NOT through `send_intent_with`: that gates a direct token against the ELECTED
/// route, and by this instant the elected route is the new one, so a retirement
/// sent through it would be dropped exactly when it is needed.
fn release_previous(
    store: &Store,
    session_id: &str,
    old_token: &TerminalToken,
    old_views: &[(String, u64)],
    out: &mut Vec<Effect>,
) {
    for (wire_view_id, revision) in old_views {
        match old_token.transport {
            TerminalTransport::Sync => {
                if store.sync_terminal_token().as_ref() != Some(old_token) {
                    // The socket redialled between the command and the answer.
                    // Stamping this generation onto the new socket would retire
                    // a view the new socket never held, so the old lease is left
                    // to expire on its own — the same thing that happens when a
                    // tab is closed.
                    tracing::debug!(
                        target: "route",
                        session_id,
                        view_id = %wire_view_id,
                        "the Sync generation this view was published on has redialled; \
                         its lease will expire rather than be stamped onto the new socket"
                    );
                    continue;
                }
                out.push(Effect::SendSync(SyncCommand::TerminalView {
                    session_id: session_id.to_string(),
                    view_id: wire_view_id.clone(),
                    intent: ViewIntent::Unpublish,
                    revision: revision + 1,
                    token: old_token.clone(),
                }));
            }
            TerminalTransport::Loopback | TerminalTransport::Peer => {
                out.push(Effect::SendDirect {
                    token: old_token.clone(),
                    command: DirectCommand::View {
                        session_id: session_id.to_string(),
                        view_id: wire_view_id.clone(),
                        intent: ViewIntent::Unpublish,
                        revision: revision + 1,
                    },
                });
            }
        }
    }
}
