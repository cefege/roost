//! The Sync-side rules: which frame may be applied, when, and what acknowledging
//! one costs.
//!
//! The rule that is not obvious and matters most: DISPATCH IS NOT GENERATION
//! GATED, THE ACK IS. A frame a live callback already accepted, or one retained
//! by the pre-hydration queue, is applied regardless of which socket generation
//! delivered it — because revoking an applied frame because the socket redialled
//! mid-batch would strand state the frame already changed, and the coordinator
//! will not replay it. Only the cumulative acknowledgement is gated to the
//! still-current, open, accepting socket.
//!
//! A DIRECT carrier's frames take the other path entirely, and the whole of it
//! is in `candidate`: a cell frame, a view-state answer and an input result are
//! three different things to do, and the first of them may only ever touch a
//! replica that has not been elected.
//!
//! Ported from `apps/web/src/client/sync/sync-flow.ts:44-65` and
//! `apps/web/src/store/sync-inbound.ts:50-90`. Contract: `protocol/spec/sync.md`;
//! the reasons are in `docs/phase4-client-contract.md` §7 and §11.

use crate::effect::{Effect, RpcResult, SyncCommand};
use crate::store::root::CoordIdentity;

mod apply_frame;
mod candidate;
mod close_failed;
mod elected;
mod fold_controls;
mod fold_registry;
mod fold_session_meta;
mod hydration;
pub mod lifecycle;
mod promotion;
mod transport_probe;

use self::apply_frame::apply_frame;
pub(crate) use self::close_failed::close_failed_sync_link;
// The one Sync-generation recovery. The terminal liveness watchdog escalates
// through it rather than opening a second path to the same redial.
pub(crate) use self::hydration::request_link_replacement;
pub(crate) use self::promotion::{
    claim_due_fallbacks, reject_sync_claims, resume_held_promotion, sweep_route_claims,
};
pub(crate) use self::transport_probe::request_transport_probe;
use crate::store::Store;
use crate::sync::SyncFrame;
use crate::sync::link::RetainedFrame;
use crate::terminal::token::TerminalToken;

/// Whether a frame is part of the negotiation rather than the data.
///
/// Controls are applied immediately even before hydration: `SyncSubscribed` is
/// what CREATES the domain state, so a control held back for hydration would
/// leave every later frame with no subscription behind it. The set is v2's
/// control lane (`apps/web/src/store/sync-inbound.ts:210-236`): every arm the
/// coordinator stamps with `delivery_seq = 0`.
pub fn is_control(frame: &SyncFrame) -> bool {
    matches!(
        frame,
        SyncFrame::Subscribed { .. }
            | SyncFrame::DomainReset { .. }
            | SyncFrame::InputResult { .. }
            | SyncFrame::InputRouteResult { .. }
            | SyncFrame::TransportProbeResult { .. }
            | SyncFrame::UiState
            | SyncFrame::UiCommand { .. }
            | SyncFrame::CoordinatorRelocation { .. }
            | SyncFrame::Keepalive
    )
}

/// Apply one frame that arrived on the Sync socket, and acknowledge it.
pub fn handle_sync_frame(
    store: &mut Store,
    generation: u64,
    delivery_seq: u64,
    frame: &SyncFrame,
    now_ms: u64,
    out: &mut Vec<Effect>,
) {
    if store.sync.note_frame(generation, now_ms) {
        // Any frame on the live link proves it works: the backoff starts over
        // (v2 `_noteSyncV2FrameReceived`).
        store.sync.redial.note_frame_received();
    }

    // Before the snapshot is in, application frames are HELD, not applied: a
    // snapshot arriving after a fold prunes the fold's session, and a terminal
    // frame folded onto a store with no views has nowhere to land.
    if !store.hydrated && !is_control(frame) {
        store.sync.retain(RetainedFrame {
            frame: frame.clone(),
            delivery_seq,
            generation,
        });
        return;
    }
    if !store.sync.may_apply(frame) {
        // An application frame before `SyncSubscribed` is a protocol violation,
        // not a race. It is NOT acknowledged: the coordinator would then release
        // records this client never applied.
        tracing::warn!(
            target: "sync",
            generation,
            frame = frame.kind_name(),
            "application frame arrived before its domain was subscribed"
        );
        return;
    }

    apply_frame(store, generation, delivery_seq, frame, now_ms, out);

    // Cumulative, and only for a frame that was actually applied. `delivery_seq`
    // is zero for a control, and a control has no window cost: acknowledging one
    // would release application records this client has not processed.
    if delivery_seq == 0 || !store.sync.accepts(generation) {
        return;
    }
    if store.sync.socket_id().is_some() {
        out.push(Effect::SendSync(SyncCommand::Ack {
            ack_delivery_seq: delivery_seq,
        }));
    }
}

/// Apply one frame that arrived on a DIRECT carrier.
///
/// The first question is WHETHER THIS SOCKET IS ELECTED, because that decides
/// which replica the frame belongs to. A cell frame for a carrier that is only
/// STAGED goes to the staged replica and nowhere else — a candidate with a
/// half-built baseline must not be able to paint
/// (`protocol/spec/direct-terminal.md:27`). A cell frame on the carrier that
/// already IS the elected route goes to the canonical replica, which is the
/// replica that commit installed. Dropping those instead is what a promoted
/// route looks like from the reader's side: the baseline painted once, at the
/// moment of the swap, and then nothing ever changed again.
///
/// A view-state answer is the same split by view id: the candidate's OWN view
/// while staging, and the pane's wire view once the route is elected. And an
/// input result is only true for a batch THIS carrier carried — a Sync batch the
/// same worker wrote before the promotion is still outstanding there, and
/// answering it from a direct frame would settle one lane twice.
pub fn handle_direct_frame(
    store: &mut Store,
    token: &TerminalToken,
    frame: &SyncFrame,
    now_ms: u64,
    out: &mut Vec<Effect>,
) {
    // A probe answer names a worker, not a session: it settles the probe this
    // exact carrier sent, whatever sessions it carries.
    if let SyncFrame::TransportProbeResult { result } = frame {
        let route = crate::store::sync_feeds::ProbeRoute::Direct(token.clone());
        transport_probe::fold_transport_probe_result(store, &route, result, now_ms);
        return;
    }
    let Some(session_id) = frame.session_id().map(str::to_string) else {
        return;
    };
    // The route is elected ON THIS TOKEN or it is not. Everything below reads
    // this one fact: it is the difference between a frame that feeds the grid and
    // a frame that feeds a replica nobody is painting from.
    let elected = store.routes.route_matches(&session_id, token);
    match frame {
        SyncFrame::CellGrid { frame: cell, .. } => {
            if elected {
                elected::fold_into_elected(store, &session_id, token, now_ms, out, |replica| {
                    replica.admit_frame(cell, false, token, now_ms)
                });
            } else {
                candidate::fold_into_candidate(store, &session_id, token, now_ms, out, |replica| {
                    replica.admit_frame(cell, false, token, now_ms)
                });
            }
        }
        SyncFrame::CellGridChunk { chunk, .. } => {
            if elected {
                elected::fold_into_elected(store, &session_id, token, now_ms, out, |replica| {
                    replica.admit_chunk(chunk, token, now_ms)
                });
            } else {
                candidate::fold_into_candidate(store, &session_id, token, now_ms, out, |replica| {
                    replica.admit_chunk(chunk, token, now_ms)
                });
            }
        }
        SyncFrame::ViewState { .. } if elected => {
            // The canonical already holds this pane's WIRE id, so the ordinary
            // correlation finds the right record; nothing about the answer is
            // direct-specific once the route is elected.
            crate::handle_terminal::handle_correlated_result(store, frame, now_ms, out);
        }
        SyncFrame::ViewState { .. } => {
            candidate::apply_direct_view_state(store, &session_id, token, frame, now_ms, out);
        }
        SyncFrame::InputResult {
            session_id,
            input_seq,
            outcome,
            ..
        } => {
            if store
                .input
                .settle_from(session_id, token, *input_seq, outcome.clone())
            {
                store.note_change();
                tracing::info!(
                    target: "terminal",
                    session_id,
                    input_seq,
                    status = outcome.status_name(),
                    "terminal input settled on the carrier that wrote it"
                );
            }
        }
        SyncFrame::InputRouteResult { result } => {
            promotion::settle_route_result(store, token, result, now_ms, out);
        }
        _ => tracing::debug!(
            target: "terminal",
            frame = frame.kind_name(),
            "direct carrier frame with no direct-carrier rule"
        ),
    }
}

/// Apply everything the pre-hydration queue held, in arrival order.
///
/// The queue is drained ONCE, and a frame arriving while it drains is retained
/// again rather than applied behind the queue's back — otherwise the order the
/// coordinator sequenced them in is lost.
pub(crate) fn hydrate(store: &mut Store, now_ms: u64, out: &mut Vec<Effect>) {
    store.hydrated = true;
    store.note_change();
    for held in store.sync.take_retained() {
        // Deliberately not generation gated: the frame was accepted before the
        // redial, and the coordinator will not send it again.
        apply_frame(
            store,
            held.generation,
            held.delivery_seq,
            &held.frame,
            now_ms,
            out,
        );
    }
}

/// Apply one Connect unary answer. The hydrators and the bootstrap probe see it
/// first; what is left is an answer some other surface asked for.
pub fn handle_rpc_result(
    store: &mut Store,
    result: &RpcResult,
    now_ms: u64,
    out: &mut Vec<Effect>,
) {
    if hydration::settle_hydration_result(store, result, now_ms, out) {
        return;
    }
    if crate::handle_close_kill::settle_kill(store, result, now_ms, out) {
        return;
    }
    match result {
        RpcResult::CoordIdentity {
            git_sha,
            public_url,
            terminal_peer_stun_urls,
            ..
        } => {
            store.coord_identity = Some(CoordIdentity {
                git_sha: git_sha.clone(),
                public_url: public_url.clone(),
            });
            store.direct.set_stun_urls(terminal_peer_stun_urls.clone());
            store.note_change();
            tracing::info!(
                target: "rpc",
                git_sha = %git_sha,
                stun_urls = ?terminal_peer_stun_urls,
                "coordinator identity"
            );
        }
        RpcResult::SearchPage {
            call_id,
            search_id,
            page,
        } => {
            // A page answers one outstanding call. Anything else is a
            // replacement the reader has already moved past, and the
            // controller is the only party that can tell the two apart.
            if store.global_search.accept_page(*call_id, search_id, page) {
                store.note_change();
            }
        }
        RpcResult::Failed { call_id, error } => {
            // A page this controller was waiting on. `in_flight` is the only
            // thing gating the next one, so a refusal that does not release it
            // leaves the search wedged for the life of the document: every
            // later `load_more` refuses and nothing else clears it. Refusals
            // belonging to no search report `false` and change nothing.
            if store.global_search.fail_page(*call_id, error.to_string()) {
                store.note_change();
                tracing::warn!(
                    target: "rpc",
                    call_id = *call_id,
                    %error,
                    "a global-search page was refused; the search can be retried"
                );
            } else {
                tracing::warn!(target: "rpc", call_id = *call_id, %error, "connect call failed");
            }
        }
        other => {
            tracing::debug!(
                target: "rpc",
                call_id = other.call_id(),
                answer = other.kind_name(),
                "answer with no pending owner"
            );
        }
    }
}
