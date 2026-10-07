//! The per-frame fold: what applying one admitted `SyncFrame` does to the store.
//!
//! Split out of `handle_sync` for the 400-line cap, not for a reason. The
//! admission and acknowledgement rules stay in the parent because that is where
//! the generation fence lives; this file is the part downstream of that fence,
//! and it is deliberately not a second gate. The registry, session-metadata and
//! control folds live in the sibling `fold_*` files; this match only routes.

use roost_protocol::wire::WorkerPresenceEvent;

use crate::effect::Effect;
use crate::store::Store;
use crate::store::frames_revision::PaintedMark;
use crate::sync::{SyncDomain, SyncFrame};
use crate::terminal::session::Admission;
use crate::terminal::smoke_faults::FaultedFrameKind;
use crate::terminal::token::TerminalTransport;

use super::fold_controls::{
    fold_audit_row, fold_coordinator_relocation, fold_pair_request, fold_ui_command,
};
use super::fold_registry::{
    fold_mcp_message, fold_task_delta, fold_worker_presence, fold_worker_routable,
    fold_workspace_delta,
};
use super::fold_session_meta::{
    fold_last_activity, fold_session_presence, fold_session_viewers, fold_terminal_title,
};
use super::hydration::trigger_hydration;
use super::transport_probe::fold_transport_probe_result;
use crate::store::sync_feeds::ProbeRoute;

/// Apply one already-admitted frame, without acknowledging it.
///
/// `delivery_seq` is the frame's own transport sequence, carried so a card a
/// frame raises is identified by the frame that raised it.
pub(super) fn apply_frame(
    store: &mut Store,
    generation: u64,
    delivery_seq: u64,
    frame: &SyncFrame,
    now_ms: u64,
    out: &mut Vec<Effect>,
) {
    match frame {
        SyncFrame::Subscribed { domains, .. } => {
            if !store.sync.install_subscribed(generation, domains) {
                return;
            }
            store.sync.hydrations.note_subscribed();
            // A new socket starts with no routable seed in progress: v2 builds a
            // fresh `routableChunks` map per `subscribed` (`sync-inbound.ts:107-112`).
            store.routable_assembly.clear();
            store.note_change();
            // The hold is PER LINK, not a one-shot latch. A new socket
            // announces fresh domain generations with none of them ready, so
            // the snapshot this client is about to receive is not in yet, and
            // `SessionPlane::apply_snapshot` is the only path that prunes.
            // Leaving the latch set past the first socket is what lets a
            // re-hydration answer land on top of rows already folded off the
            // live feed and delete a terminal that is still painting.
            //
            // Only when the terminal domain is SUBSCRIBED: a socket that
            // announces it unsubscribed will never publish a snapshot, and a
            // hold nothing can close would strand every application frame.
            store.hydrated = !domains
                .iter()
                .any(|(domain, _, subscribed)| *domain == SyncDomain::Terminal && *subscribed);
            tracing::info!(target: "sync", generation, "sync subscribed");
            // Hydrate exactly the domains the coordinator announced as
            // subscribed; a lazy domain waits for the surface that needs it
            // (`sync-inbound.ts:124-127`).
            for (domain, _, subscribed) in domains {
                if *subscribed {
                    trigger_hydration(store, *domain, now_ms, out);
                }
            }
        }
        SyncFrame::DomainReset {
            domain,
            generation: domain_generation,
            reason,
            subscribed,
        } => {
            // v2 `handleDomainReset` (`apps/web/src/store/sync-inbound.ts:131-148`):
            // the domain is never ready after a reset, and only a domain this
            // client is still subscribed to is hydrated again.
            if !store
                .sync
                .reset_domain(generation, *domain, *domain_generation, *subscribed)
            {
                return;
            }
            // A workers reset abandons any partial routable seed, as v2 clears
            // `routableChunks` (`sync-inbound.ts:140-143`).
            if *domain == SyncDomain::Workers {
                store.routable_assembly.clear();
            }
            // A terminal reset drops the snapshot the client is holding live
            // rows against, so the hold reopens for the same reason a new
            // socket's does: the next snapshot is a full replacement, and a
            // fold that lands first must be queued behind it, not pruned by it.
            // …and only while this client is still subscribed to it, which is
            // the same condition that decides whether a re-hydration follows.
            if *domain == SyncDomain::Terminal && *subscribed {
                store.hydrated = false;
            }
            store.note_change();
            tracing::info!(
                target: "sync",
                domain = domain.as_str(),
                reason = %reason,
                subscribed = *subscribed,
                "domain reset"
            );
            if *subscribed {
                trigger_hydration(store, *domain, now_ms, out);
            }
        }
        SyncFrame::SessionEvent { event, event_id } => {
            store.sessions.apply(&event.0);
            // The cursor advances in the SAME step that applied the event, so it
            // can never name an event the store has not applied.
            store.sync.watermark.note(*event_id);
            store.note_change();
            crate::handle_terminal::reconcile_prewarm(store, now_ms, out);
        }
        SyncFrame::SessionsSnapshot { sessions } => {
            store.sessions.apply_snapshot(sessions.clone());
            store.note_change();
            crate::handle_terminal::reconcile_prewarm(store, now_ms, out);
        }
        SyncFrame::CellGrid {
            session_id,
            frame: cell,
        } => {
            let Some(token) = store.sync.terminal_token() else {
                return;
            };
            if elected_direct_owns(store, session_id) {
                return;
            }
            let (full, seq) = (cell.full, Some(cell.seq));
            if store.terminal_smoke_faults.consume(
                session_id,
                &token,
                FaultedFrameKind::Frame,
                full,
                seq,
            ) {
                return;
            }
            let Some(replica) = store.terminal_mut_if_present(session_id) else {
                return;
            };
            replica.bind_generation(&token);
            let before = PaintedMark::of(replica);
            let admission = replica.admit_frame(cell, false, &token, now_ms);
            // A host learns that a pane changed from the store's frame counter
            // and reads how far the replica moved from `frame_revision`, so the
            // counter has to move too — otherwise the notification that would
            // tell it to look never arrives.
            let after = PaintedMark::of(replica);
            store.note_fold(before, after);
            if matches!(admission, Admission::Refused { latched: true, .. }) {
                crate::handle_sweep::request_repair_if_due(store, session_id, now_ms, out);
            }
        }
        SyncFrame::CellGridChunk { session_id, chunk } => {
            let Some(token) = store.sync.terminal_token() else {
                return;
            };
            if elected_direct_owns(store, session_id) {
                return;
            }
            let full = chunk.part.as_option().map(|part| part.full);
            if let Some(full) = full
                && store.terminal_smoke_faults.consume(
                    session_id,
                    &token,
                    FaultedFrameKind::Chunk,
                    full,
                    None,
                )
            {
                return;
            }
            let Some(replica) = store.terminal_mut_if_present(session_id) else {
                return;
            };
            replica.bind_generation(&token);
            let before = PaintedMark::of(replica);
            let admission = replica.admit_chunk(chunk, &token, now_ms);
            let after = PaintedMark::of(replica);
            store.note_fold(before, after);
            if matches!(admission, Admission::Refused { latched: true, .. }) {
                crate::handle_sweep::request_repair_if_due(store, session_id, now_ms, out);
            }
        }
        SyncFrame::ViewState { .. } | SyncFrame::InputResult { .. } => {
            crate::handle_terminal::handle_correlated_result(store, frame, now_ms, out);
        }
        SyncFrame::AgentStatus { update } => {
            // The coordinator already fenced this report against its own
            // arrival order; what is left is the browser-lifecycle fence and
            // the acknowledgement ledger, both of which live in the
            // projection. A refused report changes nothing, so it must not
            // move the revision a host subscribes to.
            if store
                .agent_status
                .apply_update(update, &store.agent_seen)
                .is_some()
            {
                store.note_change();
            }
        }
        SyncFrame::AgentStatusRefused { session_id, reason } => {
            tracing::warn!(target: "sync", session_id = %session_id, reason = %reason, "agent status report refused");
        }
        SyncFrame::SessionEventRejected { event_id, reason } => {
            // The cursor moves past an event the schema refused, as v2 moves it
            // before `foldEventIntoStore` rejects the shape; nothing is folded.
            if store.sync.watermark.note(*event_id) {
                store.note_change();
            }
            tracing::warn!(target: "sync", event_id = *event_id, reason = %reason, "session event rejected");
        }
        SyncFrame::SessionViewers {
            session_id,
            viewers,
        } => fold_session_viewers(store, session_id, viewers),
        SyncFrame::SessionPresence {
            session_id,
            payload,
        } => fold_session_presence(store, session_id, payload),
        SyncFrame::TerminalTitle { session_id, title } => {
            fold_terminal_title(store, session_id, title);
        }
        SyncFrame::LastActivity { session_id, ts_ms } => {
            fold_last_activity(store, session_id, *ts_ms);
        }
        SyncFrame::AuditRow { row } => fold_audit_row(store, row),
        SyncFrame::WorkspaceDelta { delta } => fold_workspace_delta(store, delta),
        SyncFrame::TaskDelta { delta } => fold_task_delta(store, delta),
        SyncFrame::McpMessage { message } => fold_mcp_message(store, message),
        SyncFrame::TerminalClipboard { session_id, text } => {
            store.terminal_clipboard_requests.push(
                crate::store::clipboard_requests::TerminalClipboardRequest {
                    session_id: session_id.clone(),
                    text: text.clone(),
                    delivery_seq,
                },
            );
            store.note_change();
        }
        SyncFrame::ClipboardHistory { change } => {
            use crate::sync::inbound::ClipboardHistoryDelta;
            match change {
                ClipboardHistoryDelta::Added(entry) => store.clipboard_history.add(entry.clone()),
                ClipboardHistoryDelta::Removed(id) => store.clipboard_history.remove(id),
                ClipboardHistoryDelta::Cleared => store.clipboard_history.replace(Vec::new()),
            }
            store.note_change();
        }
        SyncFrame::CommandFinished {
            session_id,
            exit_code,
            duration_ms,
        } => super::fold_session_meta::fold_command_finished(
            store,
            session_id,
            *exit_code,
            *duration_ms,
            delivery_seq,
        ),
                crate::handle_terminal::reconcile_prewarm(store, now_ms, out);
            }
        }
        SyncFrame::WorkerRoutable { fps, chunk } => {
            fold_worker_routable(store, generation, fps, chunk.as_ref(), out);
            crate::handle_terminal::reconcile_prewarm(store, now_ms, out);
        }
        SyncFrame::PairRequestDelta { change } => {
            fold_pair_request(store, change, delivery_seq, now_ms);
        }
        SyncFrame::UiCommand { command } => fold_ui_command(store, command),
        SyncFrame::CoordinatorRelocation { relocation } => {
            fold_coordinator_relocation(store, generation, relocation, out);
        }
        SyncFrame::InputRouteResult { result } => {
            // Settled against the Sync generation it arrived on: a claim sent
            // on a socket that has since redialled is answered for nobody.
            if let Some(token) = store.sync_terminal_token() {
                super::promotion::settle_route_result(store, &token, result, now_ms, out);
            }
        }
        SyncFrame::TransportProbeResult { result } => {
            let route = ProbeRoute::Sync {
                socket_generation: generation,
            };
            fold_transport_probe_result(store, &route, result, now_ms);
        }
        SyncFrame::UiState => {
            // Browser tabs deliberately do not project peer UI state: routing
            // and discarding it is its full consumption (`sync-frame.ts:329-333`).
            tracing::trace!(target: "sync", "peer ui state discarded");
        }
        SyncFrame::Keepalive => {
            tracing::trace!(target: "sync", frame = frame.kind_name(), "frame applied");
        }
    }
}

/// Whether an elected direct carrier owns this session's replica, which makes a
/// Sync cell frame for it a straggler.
///
/// The coordinator keeps forwarding the session's Sync stream for a while after
/// a promotion. Admitting those frames would rebind the replica to Sync, and the
/// next keystroke would leave on Sync without the route epoch the promotion
/// claimed, which the worker refuses as a changed input route. v2 drops every
/// frame whose owner is not the session's generation, and only a publication
/// moves that generation (`terminal-stream-replica.ts`); the Sync rotation after
/// a lost direct route is the publication that rebinds this replica to Sync.
fn elected_direct_owns(store: &Store, session_id: &str) -> bool {
    store
        .terminal(session_id)
        .and_then(|replica| replica.generation())
        .is_some_and(|token| token.transport != TerminalTransport::Sync)
}
