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
//! Ported from `apps/web/src/client/sync/sync-flow.ts:44-65` and
//! `apps/web/src/store/sync-inbound.ts:50-90`. Contract: `protocol/spec/sync.md`;
//! the reasons are in `docs/phase4-client-contract.md` §7 and §11.

use crate::effect::{Effect, RpcResult, SyncCommand};
use crate::store::root::CoordIdentity;

mod apply_frame;
mod close_failed;
mod fold_controls;
mod fold_registry;
mod fold_session_meta;
mod hydration;
pub mod lifecycle;

use self::apply_frame::apply_frame;
pub(crate) use self::close_failed::close_failed_sync_link;
use crate::store::Store;
use crate::sync::SyncFrame;
use crate::sync::link::RetainedFrame;
use crate::terminal::session::TerminalSession;
use crate::terminal::token::TerminalToken;
use crate::terminal::{Admission, PromotionCandidate};

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

/// Apply a frame that arrived on a DIRECT carrier.
///
/// A cell frame goes to that carrier's STAGED replica, never to the session's
/// canonical: a candidate with a half-built baseline must not be able to paint
/// (`protocol/spec/direct-terminal.md:27`). Only `promote` swaps it in.
pub fn handle_direct_frame(
    store: &mut Store,
    token: &TerminalToken,
    frame: &SyncFrame,
    now_ms: u64,
    _out: &mut Vec<Effect>,
) {
    let Some(session_id) = frame.session_id().map(str::to_string) else {
        return;
    };
    let Some(worker_fp) = token.worker_fp.clone() else {
        return;
    };
    match frame {
        SyncFrame::CellGrid { frame: cell, .. } => {
            fold_into_candidate(store, &session_id, &worker_fp, token, |replica| {
                replica.admit_frame(cell, false, token, now_ms)
            })
        }
        SyncFrame::CellGridChunk { chunk, .. } => {
            fold_into_candidate(store, &session_id, &worker_fp, token, |replica| {
                replica.admit_chunk(chunk, token, now_ms)
            })
        }
        _ => tracing::debug!(
            target: "terminal",
            frame = frame.kind_name(),
            "direct carrier frame with no direct-carrier rule"
        ),
    }
}

/// Fold one direct-carrier frame into the session's staged replica.
///
/// The attempt id is allocated ONCE per staging, not per frame: it exists so a
/// slow fold cannot overwrite a newer one, and a fresh id per frame would make
/// every frame a newer one.
///
/// The replica is folded IN PLACE. A `TerminalSession` owns a chunk assembler and
/// cannot be cloned, so it is created once on the first frame of a staging and
/// borrowed mutably by every frame after — which is also the only way a candidate
/// can accumulate a baseline instead of restarting it per frame.
fn fold_into_candidate<F>(
    store: &mut Store,
    session_id: &str,
    worker_fp: &str,
    token: &TerminalToken,
    fold: F,
) where
    F: FnOnce(&mut TerminalSession) -> Admission,
{
    // Copied out and OWNED before the store is touched again: holding a borrow
    // into `store.routes` across `store.next_attempt_id += 1` would be two live
    // borrows of one struct.
    let staged_attempt = store
        .routes
        .candidate(session_id)
        .map(|candidate| candidate.attempt_id);
    let attempt_id = staged_attempt.unwrap_or_else(|| {
        let attempt_id = store.next_attempt_id;
        store.next_attempt_id += 1;
        attempt_id
    });
    if store.routes.staged_replica_mut(session_id).is_none() {
        let connection_id = connection_id_for(store, token);
        store.routes.stage(
            PromotionCandidate {
                session_id: session_id.to_string(),
                connection_id,
                token: token.clone(),
                attempt_id,
                baseline_ready: false,
            },
            TerminalSession::new(session_id, worker_fp),
        );
        store.note_change();
    }
    let (painted_before, painted_after, baseline_ready) = {
        let Some(replica) = store.routes.staged_replica_mut(session_id) else {
            return;
        };
        replica.bind_generation(token);
        let before = replica.frame_revision();
        let _ = fold(replica);
        (before, replica.frame_revision(), replica.baseline_ready())
    };
    store
        .routes
        .mark_candidate_baseline(session_id, baseline_ready);
    if painted_after != painted_before {
        store.note_change();
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
pub fn handle_rpc_result(store: &mut Store, result: &RpcResult, now_ms: u64, out: &mut Vec<Effect>) {
    if hydration::settle_hydration_result(store, result, now_ms, out) {
        return;
    }
    match result {
        RpcResult::CoordIdentity {
            git_sha,
            public_url,
            ..
        } => {
            store.coord_identity = Some(CoordIdentity {
                git_sha: git_sha.clone(),
                public_url: public_url.clone(),
            });
            store.note_change();
            tracing::info!(target: "rpc", git_sha = %git_sha, "coordinator identity");
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
            tracing::warn!(target: "rpc", call_id = *call_id, %error, "connect call failed");
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

/// The host's connection id for a carrier token, or empty when the registry has
/// not seen it. Empty is safe: the registry refuses a promotion whose connection
/// it cannot find, so an unknown carrier stages and never promotes.
fn connection_id_for(store: &Store, token: &TerminalToken) -> String {
    let Some(worker_fp) = token.worker_fp.as_deref() else {
        return String::new();
    };
    store
        .routes
        .route_connection_for(worker_fp, token.transport)
        .unwrap_or_default()
}
