//! The phase marks a Sync frame stands for, recorded as the pump delivers it:
//! the subscription, the session snapshot, a session's first cell frame and an
//! accepted view. Called from `pump::socket::deliver`; writes the document's
//! phase ring. Ports the `markPhase` calls of v2 `store/sync-inbound.ts`,
//! `store/sync-bootstrap.ts`, `store/sync-domain-state.ts`,
//! `store/terminal-stream-replica.ts` and `store/terminal-stream-view-commands.ts`.

use roost_client_core::{ClientEvent, SyncFrame};
use serde_json::json;

use super::phase_marks::{PhaseName, mark_phase, mark_phase_once};

/// Record the mark `event`, delivered on socket `generation`, stands for.
pub fn mark_sync_delivery(generation: u64, event: &ClientEvent) {
    let ClientEvent::SyncFrameReceived { frame, .. } = event else {
        return;
    };
    match frame {
        SyncFrame::Subscribed { process_epoch, .. } => mark_phase(
            PhaseName::SyncSubscribed,
            &[
                ("generation", json!(generation)),
                ("processEpoch", json!(process_epoch)),
            ],
        ),
        SyncFrame::SessionsSnapshot { sessions } => {
            mark_phase(
                PhaseName::SnapshotApplied,
                &[
                    ("domain", json!("SESSIONS")),
                    ("generation", json!(generation)),
                ],
            );
            mark_phase(
                PhaseName::SessionsListPublish,
                &[
                    ("socketGeneration", json!(generation)),
                    ("sessions", json!(sessions.len())),
                ],
            );
        }
        SyncFrame::CellGrid { session_id, frame } => mark_phase_once(
            PhaseName::FirstCellReceive,
            session_id,
            &[
                ("sessionId", json!(session_id)),
                ("sequence", json!(frame.seq)),
                ("full", json!(frame.full)),
            ],
        ),
        SyncFrame::ViewState {
            session_id,
            accepted: true,
            effective_cols,
            effective_rows,
            ..
        } => mark_phase(
            PhaseName::ViewportAccept,
            &[
                ("sessionId", json!(session_id)),
                ("cols", json!(effective_cols)),
                ("rows", json!(effective_rows)),
            ],
        ),
        _ => {}
    }
}
