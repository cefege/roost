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

use crate::effect::{Effect, RpcCall};
use crate::event::ClientEvent;
use crate::handle_input::handle_terminal_input;
use crate::handle_sweep::handle_sweep;
use crate::handle_sync::{handle_direct_frame, handle_rpc_result, handle_sync_frame, hydrate};
use crate::handle_terminal::{
    ViewOpen, handle_carrier_lost, handle_carrier_ready, handle_search_page, handle_view_closed,
    handle_view_hidden, handle_view_opened, handle_view_resized, handle_view_state,
    handle_worker_retired,
};
use crate::platform::{Clock, KeyValueStore};
use crate::store::Store;

/// Apply one event to the store and collect what the host should now do.
///
/// The effects land in `out` in decision order. A host performs them in that
/// order; two of them in one call are a sequence, not a set.
pub fn handle_event(
    store: &mut Store,
    event: &ClientEvent,
    clock: &dyn Clock,
    storage: &dyn KeyValueStore,
    out: &mut Vec<Effect>,
) {
    // The one clock read. Every deadline below is a pure function of this number,
    // so a test can move time and a host cannot have two deadlines disagree about
    // what time it is.
    let host_now_ms = clock.now_ms();
    match event {
        // ---- dialing ----------------------------------------------------------
        ClientEvent::DialRequested => {
            if store.sync.auth_revoked {
                // The coordinator refused this credential. Dialing again would
                // present the same rejected credential, so the request is declined
                // rather than turned into a loop.
                tracing::warn!(target: "sync", "dial declined: the credential was revoked");
                return;
            }
            let (generation, dial) = store.sync.begin_dial(&store.tab_id);
            out.push(Effect::DialSync { generation, dial });
        }
        ClientEvent::BootstrapRequested => {
            // The session list is what issues the one-time terminal snapshot token,
            // and terminal hydration cannot be admitted without it
            // (`protocol/spec/sync.md:28`), so identity and sessions are asked for
            // together and the worker registry with them.
            out.push(Effect::Rpc(RpcCall::CoordIdentity {
                call_id: store.next_call_id(),
            }));
            out.push(Effect::Rpc(RpcCall::SessionsList {
                call_id: store.next_call_id(),
            }));
            out.push(Effect::Rpc(RpcCall::WorkersList {
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
                tracing::info!(
                    target: "sync",
                    generation = *generation,
                    socket_id = %socket_id,
                    "sync link open"
                );
            }
        }
        ClientEvent::SyncLinkClosed {
            generation,
            close_code,
        } => {
            if store.sync.close_link(*generation, *close_code) && store.sync.auth_revoked {
                tracing::error!(
                    target: "sync",
                    generation = *generation,
                    "sync credential revoked; not redialing"
                );
            }
        }
        ClientEvent::SyncFrameReceived {
            generation,
            delivery_seq,
            frame,
        } => handle_sync_frame(store, *generation, *delivery_seq, frame, host_now_ms, out),
        ClientEvent::DirectFrameReceived { token, frame } => {
            handle_direct_frame(store, token, frame, host_now_ms, out);
        }
        ClientEvent::HydrationCompleted { .. } => hydrate(store, host_now_ms, out),

        // ---- Connect and the pairing ceremony ----------------------------------
        ClientEvent::RpcResultReceived(result) => handle_rpc_result(store, result),
        ClientEvent::ChallengeSigned { account_id, .. } => {
            store.account_id = Some(account_id.clone());
        }
        ClientEvent::CredentialsDiscarded => {
            store.account_id = None;
            store.sessions = crate::sessions::SessionPlane::new();
            store.find_results.clear();
            // The recovery cursor goes with the credential. A persisted global
            // cursor would make the next socket's initial history invisible
            // (`apps/web/src/store/sync-frame.ts:55-69`).
            store.sync.watermark.reset(storage);
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
            handle_view_hidden(store, session_id, view_id, out);
        }
        ClientEvent::ViewClosed {
            session_id,
            view_id,
        } => {
            handle_view_closed(store, session_id, view_id, out);
        }
        ClientEvent::ViewStateReceived { state, .. } => {
            handle_view_state(store, state, host_now_ms, out);
        }

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
        } => crate::handle_input::handle_input_result(store, session_id, *input_seq, outcome),

        // ---- carriers ---------------------------------------------------------
        ClientEvent::CarrierReady(carrier) => handle_carrier_ready(store, carrier),
        ClientEvent::CarrierLost { connection_id } => {
            handle_carrier_lost(store, connection_id, out);
        }
        ClientEvent::WorkerRetired { worker_fp } => handle_worker_retired(store, worker_fp, out),

        // ---- find paging ------------------------------------------------------
        ClientEvent::SearchPageReceived {
            session_id,
            page,
            matches,
            before_row,
        } => handle_search_page(store, session_id, page, matches, *before_row),

        // ---- time -------------------------------------------------------------
        ClientEvent::Sweep { now_ms } => {
            // The event carries the host's reading, which may be later than the one
            // taken above. Use the later one: a sweep that fired later must not be
            // evaluated against an earlier instant, or a deadline set for "now"
            // would be a heartbeat in the past.
            handle_sweep(store, (*now_ms).max(host_now_ms), out);
        }
    }
}
