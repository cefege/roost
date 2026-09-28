//! The per-frame fold: what applying one admitted `SyncFrame` does to the store.
//!
//! Split out of `handle_sync` for the 400-line cap, not for a reason. The
//! admission and acknowledgement rules stay in the parent because that is where
//! the generation fence lives; this file is the part downstream of that fence,
//! and it is deliberately not a second gate. The registry, session-metadata and
//! control folds live in the sibling `fold_*` files; this match only routes.

use crate::effect::{Effect, SyncCommand};
use crate::store::Store;
use crate::sync::{SyncDomain, SyncFrame};

use super::fold_controls::{
    fold_audit_row, fold_coordinator_relocation, fold_input_route_result, fold_pair_request,
    fold_transport_probe_result, fold_ui_command,
};
use super::fold_registry::{
    fold_mcp_message, fold_task_delta, fold_worker_presence, fold_worker_routable,
    fold_workspace_delta,
};
use super::fold_session_meta::{
    fold_last_activity, fold_session_presence, fold_session_viewers, fold_terminal_title,
};

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
            store.sync.install_subscribed(generation, domains);
            // A new socket starts with no routable seed in progress: v2 builds a
            // fresh `routableChunks` map per `subscribed` (`sync-inbound.ts:107-112`).
            store.routable_assembly.clear();
            store.note_change();
            // Subscribe to exactly the domains the coordinator did not report as
            // already subscribed. Asking for a set other than the announced one
            // is how a client ends up with a terminal domain it never subscribed
            // to, and a frame on that domain is then a protocol violation.
            for (domain, domain_generation, already) in domains {
                if !already {
                    out.push(Effect::SendSync(SyncCommand::Subscribe {
                        domain: *domain,
                        generation: *domain_generation,
                    }));
                }
            }
        }
        SyncFrame::DomainReady {
            domain,
            generation: domain_generation,
            snapshot_token,
        } => {
            if store.sync.domain_generation(*domain) != Some(*domain_generation) {
                // A ready frame for a superseded generation is stale, not a reset.
                tracing::debug!(
                    target: "sync",
                    domain = domain.as_str(),
                    "domain_ready for a superseded generation"
                );
                return;
            }
            match store
                .sync
                .mark_domain_ready(generation, *domain, snapshot_token.as_deref())
            {
                Ok(()) => out.push(Effect::SendSync(SyncCommand::DomainReady {
                    domain: *domain,
                    generation: *domain_generation,
                    snapshot_token: snapshot_token.clone(),
                })),
                Err(reason) => {
                    // Only a subscribed domain is sent a `domain_ready` to refuse.
                    store
                        .sync
                        .reset_domain(generation, *domain, *domain_generation, true);
                    store.note_change();
                    tracing::warn!(
                        target: "sync",
                        domain = domain.as_str(),
                        reason,
                        "domain_ready refused"
                    );
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
            store.note_change();
            tracing::info!(
                target: "sync",
                domain = domain.as_str(),
                reason = %reason,
                subscribed = *subscribed,
                "domain reset"
            );
            if *subscribed {
                out.push(Effect::HydrateDomain {
                    domain: *domain,
                    generation: *domain_generation,
                });
            }
        }
        SyncFrame::SessionEvent { event, event_id } => {
            store.sessions.apply(&event.0);
            // The cursor advances in the SAME step that applied the event, so it
            // can never name an event the store has not applied.
            store.sync.watermark.note(*event_id);
            store.note_change();
        }
        SyncFrame::SessionsSnapshot { sessions } => {
            store.sessions.apply_snapshot(sessions.clone());
            store.note_change();
        }
        SyncFrame::CellGrid {
            session_id,
            frame: cell,
        } => {
            let Some(token) = store.sync.terminal_token() else {
                return;
            };
            let Some(replica) = store.terminal_mut_if_present(session_id) else {
                return;
            };
            replica.bind_generation(&token);
            let painted_before = replica.frame_revision();
            let _ = replica.admit_frame(cell, false, &token, now_ms);
            // A host learns that a pane changed from the STORE revision and
            // reads how far the replica moved from `frame_revision`, so the
            // store revision has to move too — otherwise the notification that
            // would tell it to look never arrives.
            if replica.frame_revision() != painted_before {
                store.note_change();
            }
        }
        SyncFrame::CellGridChunk { session_id, chunk } => {
            let Some(token) = store.sync.terminal_token() else {
                return;
            };
            let Some(replica) = store.terminal_mut_if_present(session_id) else {
                return;
            };
            replica.bind_generation(&token);
            let painted_before = replica.frame_revision();
            let _ = replica.admit_chunk(chunk, &token, now_ms);
            if replica.frame_revision() != painted_before {
                store.note_change();
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
        SyncFrame::WorkerPresence { event } => fold_worker_presence(store, event, out),
        SyncFrame::WorkerRoutable { fps, chunk } => {
            fold_worker_routable(store, generation, fps, chunk.as_ref(), out);
        }
        SyncFrame::PairRequestDelta { change } => {
            fold_pair_request(store, change, delivery_seq, now_ms);
        }
        SyncFrame::UiCommand { command } => fold_ui_command(store, command),
        SyncFrame::CoordinatorRelocation { relocation } => {
            fold_coordinator_relocation(store, generation, relocation, out);
        }
        SyncFrame::InputRouteResult { result } => fold_input_route_result(store, result),
        SyncFrame::TransportProbeResult { result } => {
            fold_transport_probe_result(store, generation, result, now_ms);
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
