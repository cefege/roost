//! The single entry point: one event in, the effects it owes out.
//!
//! This match IS the client's behaviour. It has no rules of its own — it routes
//! each event to the module that owns the rule, in `handle_sync`,
//! `handle_terminal` and `handle_input`, and appends what they decide to one
//! ordered vector.
//!
//! Two things happen here that are not "routing", and both are deliberate:
//! `Sweep` resolves the host's reading ONCE and hands the same instant to every
//! deadline it touches, and nothing here ever acknowledges a frame that was not
//! applied.
//!
//! Contract: `docs/phase4-client-contract.md` §3. Called only by
//! `ClientCore::handle`.

use crate::client::client_state::ClientStateStore;
use crate::effect::{Effect, RpcCall};
use crate::event::ClientEvent;
use crate::handle_input::handle_terminal_input;
use crate::handle_sweep::handle_sweep;
use crate::handle_sync::lifecycle::{
    dial, on_link_closed, on_transport_control, on_visibility, on_wake, sweep_sync,
};
use crate::handle_sync::{
    close_failed_sync_link, handle_direct_frame, handle_rpc_result, handle_sync_frame,
    handle_worker_live_subscribe, reject_sync_claims, resend_worker_live_subscription,
};
use crate::handle_terminal::{
    MintedViewId, ViewOpen, handle_carrier_authenticated, handle_carrier_lost, handle_grant_minted,
    handle_search_page, handle_view_closed, handle_view_hidden, handle_view_id_minted,
    handle_view_opened, handle_view_resized, handle_view_state, handle_worker_retired,
};
use crate::platform::Clock;
use crate::store::Store;

/// Apply one event to the store and collect what the host should now do.
///
/// The effects land in `out` in decision order. A host performs them in that
/// order; two of them in one call are a sequence, not a set.
pub fn handle_event(
    store: &mut Store,
    event: &ClientEvent,
    clock: &dyn Clock,
    storage: &ClientStateStore,
    out: &mut Vec<Effect>,
) {
    // The one clock read. Every deadline below is a pure function of this number,
    // so a test can move time and a host cannot have two deadlines disagree about
    // what time it is.
    let host_now_ms = clock.now_ms();
    match event {
        // ---- dialing ----------------------------------------------------------
        ClientEvent::DialRequested => dial(store, host_now_ms, out),
        ClientEvent::BootstrapRequested => {
            // Identity is the only serial prerequisite: every authoritative list
            // is a domain hydration started by the `subscribed` announcement, so
            // nothing is published before the socket installs its generations
            // (`apps/web/src/store/sync-bootstrap.ts:162-199`).
            out.push(Effect::Rpc(RpcCall::CoordIdentity {
                call_id: store.next_call_id(),
            }));
        }

        // ---- Sync socket -------------------------------------------------------
        ClientEvent::SyncLinkOpened {
            generation,
            socket_id,
            process_epoch,
        } => {
            if store.sync.open_link(
                *generation,
                socket_id.clone(),
                process_epoch.clone(),
                host_now_ms,
            ) {
                store.note_change();
                tracing::info!(
                    target: "sync",
                    generation = *generation,
                    socket_id = %socket_id,
                    "sync link open"
                );
                crate::client::client_state::request_list(store, out);
                resend_worker_live_subscription(store, out);
            }
        }
        ClientEvent::SyncLinkClosed {
            generation,
            close_code,
            close_reason,
        } => {
            on_link_closed(store, *generation, *close_code, close_reason, host_now_ms);
            reject_sync_claims(store, *generation, host_now_ms, out);
        }
        ClientEvent::SyncFrameReceived {
            generation,
            delivery_seq,
            frame,
        } => handle_sync_frame(store, *generation, *delivery_seq, frame, host_now_ms, out),
        ClientEvent::WorkerLiveSubscribe { worker_fps } => {
            handle_worker_live_subscribe(store, worker_fps, out);
        }
        ClientEvent::SyncFrameRefused { generation, reason } => {
            close_failed_sync_link(store, *generation, reason.clone(), out);
        }
        ClientEvent::DirectFrameReceived { token, frame } => {
            handle_direct_frame(store, token, frame, host_now_ms, out);
        }
        ClientEvent::PageVisibilityChanged { visible } => {
            on_visibility(store, *visible, host_now_ms, out);
        }
        ClientEvent::SyncWakeRequested { allow_hidden } => {
            on_wake(store, *allow_hidden, host_now_ms, out);
        }
        ClientEvent::SyncTransportControl(control) => {
            on_transport_control(store, *control, host_now_ms, out);
        }

        // ---- Connect and the pairing ceremony ----------------------------------
        ClientEvent::RpcResultReceived(result) => {
            handle_rpc_result(store, storage, result, host_now_ms, out)
        }
        ClientEvent::CredentialsDiscarded => {
            store.account_id = None;
            store.sessions = crate::sessions::SessionPlane::new();
            store.find_results.clear();
            // The recovery cursor goes with the credential. A persisted global
            // cursor would make the next socket's initial history invisible
            // (`apps/web/src/store/sync-frame.ts:55-69`).
            store.sync.watermark.reset(storage);
            store.terminal_clipboard_requests.clear();
            store.clipboard_history.clear();
            store.command_finished_requests.clear();
            store.terminal_signals.clear();
            store.note_change();
            // No session is left to keep a peer warm for.
            crate::handle_terminal::reconcile_prewarm(store, host_now_ms, out);
        }

        // ---- terminal views ---------------------------------------------------
        ClientEvent::ViewOpened {
            session_id,
            worker_fp,
            view_id,
            cols,
            rows,
        } => handle_view_opened(
            store,
            ViewOpen {
                session_id,
                worker_fp,
                view_id,
                cols: *cols,
                rows: *rows,
            },
            host_now_ms,
            out,
        ),
        ClientEvent::ViewResized {
            session_id,
            view_id,
            cols,
            rows,
        } => handle_view_resized(store, session_id, view_id, *cols, *rows, host_now_ms, out),
        ClientEvent::ViewHidden {
            session_id,
            view_id,
        } => {
            handle_view_hidden(store, session_id, view_id, host_now_ms, out);
        }
        ClientEvent::ViewClosed {
            session_id,
            view_id,
        } => {
            handle_view_closed(store, session_id, view_id, host_now_ms, out);
        }
        ClientEvent::ViewStateReceived { state, .. } => {
            handle_view_state(store, state, host_now_ms, out);
        }
        ClientEvent::TerminalViewIdMinted {
            session_id,
            attempt_id,
            logical_view_id,
            target,
            wire_view_id,
        } => handle_view_id_minted(
            store,
            MintedViewId {
                session_id,
                attempt_id: *attempt_id,
                logical_view_id,
                target: *target,
                wire_view_id: wire_view_id.as_deref(),
            },
            host_now_ms,
            out,
        ),

        // ---- terminal input ---------------------------------------------------
        ClientEvent::TerminalInput {
            session_id,
            view_id,
            bytes,
        } => handle_terminal_input(
            store,
            session_id,
            view_id.as_deref(),
            bytes,
            host_now_ms,
            out,
        ),
        ClientEvent::InputResultReceived {
            session_id,
            input_seq,
            outcome,
            ..
        } => crate::handle_input::handle_input_result(
            store,
            session_id,
            *input_seq,
            outcome,
            host_now_ms,
            out,
        ),

        // ---- carriers ---------------------------------------------------------
        ClientEvent::CarrierReady(carrier) => {
            handle_carrier_authenticated(store, carrier, host_now_ms, out);
        }
        ClientEvent::TransportProbeRequested {
            session_id,
            request_id,
        } => crate::handle_sync::request_transport_probe(
            store,
            session_id,
            request_id,
            host_now_ms,
            out,
        ),
        ClientEvent::CarrierLost { connection_id } => {
            handle_carrier_lost(store, connection_id, out);
        }
        ClientEvent::WorkerRetired { worker_fp } => handle_worker_retired(store, worker_fp, out),
        ClientEvent::LocalDoorAnswered {
            worker_fp,
            serving_worker_fp,
        } => {
            store
                .direct
                .local_door_answered(worker_fp, serving_worker_fp, out);
        }
        ClientEvent::DirectGrantMinted { grant } => {
            handle_grant_minted(store, grant, host_now_ms, out);
        }
        ClientEvent::DirectGrantRefused { worker_fp, reason } => {
            store
                .direct
                .grant_refused(worker_fp, host_now_ms, reason, out);
        }
        ClientEvent::CarrierTransportObserved {
            worker_fp,
            observation,
        } => {
            store
                .direct
                .transport_observed(worker_fp, observation.clone(), host_now_ms, out);
        }

        // ---- find paging ------------------------------------------------------
        ClientEvent::SearchPageReceived {
            session_id,
            page,
            matches,
            before_row,
        } => handle_search_page(store, session_id, page, matches, *before_row),

        ClientEvent::Shell(intent) => {
            crate::store::shell_intent::apply_shell_intent(store, storage, intent, host_now_ms);
        }
        ClientEvent::Sidebar(intent) => {
            crate::store::sidebar::apply_sidebar_intent(store, storage, intent);
        }
        ClientEvent::Deck(intent) => {
            crate::deck::intent::apply_deck_intent(store, intent, storage, host_now_ms);
        }

        // ---- time -------------------------------------------------------------
        ClientEvent::Sweep { now_ms } => {
            // The event carries the host's reading, which may be later than the one
            // taken above. Use the later one: a sweep that fired later must not be
            // evaluated against an earlier instant, or a deadline set for "now"
            // would be a heartbeat in the past.
            let now_ms = (*now_ms).max(host_now_ms);
            sweep_sync(store, now_ms, out);
            handle_sweep(store, storage, now_ms, out);
        }
    }
}
