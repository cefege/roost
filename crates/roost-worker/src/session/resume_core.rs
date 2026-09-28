//! Rebuilding the CORE a survivor is adopted as: the cold core at the
//! history's base geometry, the ordered replay that fills it, and the rule
//! that the geometry the keeper reports is the only acceptable result.
//! `session::resume::SessionManager::adopt_survivor` calls the one entry point
//! here. Depends on `roost_keeper` for the history vocabulary and `roost_term`
//! for the core, and on nothing in `session::resume` but its own types.
//!
//! THE REPLAY IS NOT A FLAT REPLAY. A resize reflows the lines above it, so
//! applying only the final geometry paints a screen the user is not looking
//! at; each marker is honoured as it is reached, and a core that refuses the
//! marker's size is a REFUSAL rather than a warning, because a core whose
//! parser state disagrees with the geometry is the hole adoption exists to
//! avoid.

use roost_keeper::history::HistoryRecord;
use roost_keeper::payloads::TerminalState;
use roost_term::AlacrittyCore;
use roost_term::TerminalCore;

use super::ids::mint_uuid;
use super::keeper_channels::SurvivorHistory;
use super::lifecycle::SessionManager;
use super::resize::pin_for_adoption;
use super::resume::{AdoptRefusal, AdoptionRequest};
use super::ring::ScrollbackRing;
use super::stream_scan;
use super::types::{SessionIdentity, SessionRecord};

impl SessionManager {
    /// The record a survivor becomes: a COLD core at the history's base
    /// geometry, replayed in order, its window seeded at the keeper's head.
    pub(super) fn adopted_record(
        &self,
        request: &AdoptionRequest,
        history: &SurvivorHistory,
        applied: &TerminalState,
        child_pid: u32,
    ) -> Result<SessionRecord, AdoptRefusal> {
        let channel = request.channel_id.as_u32() as u16;
        let mut core = AlacrittyCore::new(history.base_cols, history.base_rows);
        replay_ordered(&mut core, history, channel)?;
        if core.cols() != applied.cols || core.rows() != applied.rows {
            return Err(AdoptRefusal::Unreplayable {
                channel,
                reason: format!(
                    "the replay ended at {}x{} and the keeper reports {}x{}",
                    core.cols(),
                    core.rows(),
                    applied.cols,
                    applied.rows
                ),
            });
        }
        let window = history.window();
        let alt = stream_scan::scan_alt_mode(&window, false);
        // Primed, not inferred: the coordinator's snapshot reads the CORE's alt
        // state and an empty core answers false, so a live alt redraw would land
        // on the main screen. A SIGWINCH is not the repair: a TUI repaints alt
        // without re-sending `?1049h`.
        if alt && !core.using_alt_screen() {
            core.write(stream_scan::ALT_ENTER_SEQUENCES[0]);
        }
        let mut record = SessionRecord::new(
            SessionIdentity {
                session_id: request.session_id.clone(),
                channel_id: request.channel_id,
                socket_path: request.socket_path.clone(),
                cwd: request.folder.clone(),
                shell_spec: request.shell_spec.clone(),
                session_trace_id: request.session_trace_id.clone(),
                spawned_at_ms: request.now_ms,
            },
            request.close_reservation,
            Box::new(core),
            // A FRESH grid epoch: the core is new, and a client must not merge
            // the grid it holds into one that never parsed those bytes. The
            // STREAM generation is the coordinator's, so frames are addressed
            // where it expects them.
            roost_term::CellEmitState::new(
                mint_uuid().unwrap_or_else(|_| UNCORRELATED_GRID_EPOCH.to_string()),
                request.stream_id.clone(),
            ),
            ScrollbackRing::default(),
        );
        // Both numbers together, through the one writer of the floor: these
        // bytes were produced by a process that is not this record's, and floor
        // and head must agree on the very first frame — which is when a client's
        // absolute row indexes are established.
        record.adopt_retained_history(&window, history.head_seq);
        record.alt_mode = alt;
        record.child_pid = Some(child_pid);
        record.sb_origin_pin = Some(pin_for_adoption(
            request.mono_ms,
            applied.cols,
            applied.rows,
            history.evicted(),
            record.terminal_core.discarded_line_count().unwrap_or(0),
            record.terminal_core.scrollback_count() as u64,
        ));
        Ok(record)
    }
}

/// Replay a survivor's records into a COLD core, in order. The geometry markers
/// are why this is not [`super::scrollback::replay_retained_into`]: a resize
/// reflows the lines above it, so a flat replay with only the final geometry
/// paints a screen the user is not looking at.
fn replay_ordered(
    core: &mut AlacrittyCore,
    history: &SurvivorHistory,
    channel: u16,
) -> Result<(), AdoptRefusal> {
    for record in &history.records {
        match record {
            HistoryRecord::Output { bytes, .. } => core.write(bytes),
            HistoryRecord::Resize { cols, rows, .. } => {
                core.resize(*cols, *rows);
                if core.cols() != *cols || core.rows() != *rows {
                    return Err(AdoptRefusal::Unreplayable {
                        channel,
                        reason: format!(
                            "the core kept {}x{} through a {}x{} marker",
                            core.cols(),
                            core.rows(),
                            cols,
                            rows
                        ),
                    });
                }
            }
        }
    }
    Ok(())
}

/// The grid epoch a record gets when the entropy source cannot be read: a valid
/// uuid, and visibly the uncorrelated case rather than a plausible id nobody
/// can search for.
const UNCORRELATED_GRID_EPOCH: &str = "00000000-0000-4000-8000-000000000000";
