//! The per-frame fold: what applying one admitted `SyncFrame` does to the store.
//!
//! Split out of `handle_sync` for the 400-line cap, not for a reason. The
//! admission and acknowledgement rules stay in the parent because that is where
//! the generation fence lives; this file is the part downstream of that fence,
//! and it is deliberately not a second gate.

use crate::effect::{Effect, SyncCommand};
use crate::store::Store;
use crate::sync::SyncFrame;

/// Apply one already-admitted frame, without acknowledging it.
pub(super) fn apply_frame(
    store: &mut Store,
    generation: u64,
    frame: &SyncFrame,
    now_ms: u64,
    out: &mut Vec<Effect>,
) {
    match frame {
        SyncFrame::Subscribed { domains, .. } => {
            store.sync.install_subscribed(generation, domains);
            store.note_change();
            // Subscribe to exactly the domains the coordinator did not report as
            // already subscribed. Asking for a set other than the announced one
            // is how a client ends up with a terminal domain it never subscribed
            // to, and a frame on that domain is then a protocol violation.
            for (domain, _, already) in domains {
                if !already {
                    out.push(Effect::SendSync(SyncCommand::Subscribe { domain: *domain }));
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
                    snapshot_token: snapshot_token.clone(),
                })),
                Err(reason) => {
                    store
                        .sync
                        .reset_domain(generation, *domain, *domain_generation);
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
        } => {
            store
                .sync
                .reset_domain(generation, *domain, *domain_generation);
            store.note_change();
            tracing::info!(
                target: "sync",
                domain = domain.as_str(),
                reason = %reason,
                "domain reset"
            );
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
            if store.agent_status.apply_update(update, &store.agent_seen).is_some() {
                store.note_change();
            }
        }
        SyncFrame::Keepalive | SyncFrame::Unknown { .. } => {
            tracing::trace!(target: "sync", frame = frame.kind_name(), "frame applied");
        }
    }
}
