//! Rebuilding the CORE a survivor is adopted as: the cold core at the
//! history's base geometry, the ordered replay that fills it, and the rule
//! that the geometry the keeper reports is the only acceptable result. Ports
//! the core half of `apps/worker/src/session/session-resume.ts:130-260`.
//! `session::resume::SessionManager::adopt_survivor` calls the one entry point
//! here. Depends on `roost_keeper` for the history vocabulary and `roost_term`
//! for the core.
//!
//! THE REPLAY IS NOT A FLAT REPLAY. A resize reflows the lines above it, so
//! applying only the final geometry paints a screen the user is not looking
//! at; each marker is honoured as it is reached, and a core that refuses the
//! marker's size is a REFUSAL rather than a warning.

use roost_keeper::history::HistoryRecord;
use roost_keeper::payloads::TerminalState;
use roost_protocol::viewport::{TerminalGeometry, is_terminal_geometry};
use roost_protocol::wire::brand::TraceId;
use roost_term::AlacrittyCore;
use roost_term::TerminalCore;

use super::ids::{mint_trace_id, mint_uuid};
use super::keeper_channels::SurvivorHistory;
use super::lifecycle::SessionManager;
use super::replay_align::skip_orphan_sequence_prefix;
use super::resize_pin::pin_for_adoption;
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
        now_ms: i64,
    ) -> Result<SessionRecord, AdoptRefusal> {
        let channel = request.channel_id.as_u32() as u16;
        let refuse = |reason: String| AdoptRefusal::Unreplayable { channel, reason };
        if !geometry_ok(applied.cols, applied.rows) {
            return Err(refuse(
                "keeper did not report valid terminal geometry for adoption".to_owned(),
            ));
        }
        if !geometry_ok(history.base_cols, history.base_rows) {
            return Err(refuse(
                "keeper history reported invalid base terminal geometry".to_owned(),
            ));
        }
        let mut core = AlacrittyCore::new(history.base_cols, history.base_rows);
        if core.cols() != history.base_cols || core.rows() != history.base_rows {
            return Err(refuse(
                "terminal core did not retain keeper history base geometry".to_owned(),
            ));
        }
        replay_ordered(&mut core, history).map_err(refuse)?;
        if core.cols() != applied.cols || core.rows() != applied.rows {
            // v2 session-resume.ts:212-217, word for word.
            return Err(refuse(
                "ordered keeper history did not converge to reported terminal geometry".to_owned(),
            ));
        }
        let window = history.window();
        let alt = !window.is_empty() && stream_scan::scan_alt_mode(&window, false);
        // Primed, not inferred: the snapshot reads the CORE's alt state and an
        // empty core answers false, so a live alt redraw would land on the main
        // screen. Replayed probe replies are dropped by `write` itself.
        if alt && !core.using_alt_screen() {
            core.write(stream_scan::ALT_ENTER_SEQUENCES[0]);
        }
        let trace = mint_trace_id()
            .ok()
            .and_then(|value| TraceId::try_from(value).ok())
            .ok_or_else(|| refuse("a session trace id could not be minted".to_owned()))?;
        let grid_epoch = mint_uuid()
            .map_err(|error| refuse(format!("a grid epoch could not be minted: {error}")))?;
        let stream_id = mint_uuid()
            .map_err(|error| refuse(format!("a stream id could not be minted: {error}")))?;
        let mut record = SessionRecord::new(
            SessionIdentity {
                session_id: request.session_id.clone(),
                channel_id: request.channel_id,
                socket_path: format!("mux:{channel}"),
                cwd: request.folder.clone(),
                shell_spec: request.shell_spec.clone(),
                session_trace_id: trace,
                spawned_at_ms: now_ms,
            },
            request.close_reservation,
            Box::new(core),
            roost_term::CellEmitState::new(grid_epoch, stream_id),
            ScrollbackRing::default(),
        );
        // Floor and head through the one writer of the floor: these bytes were
        // produced by a process that is not this record's, and both must agree
        // on the very first frame.
        record.adopt_retained_history(&window, history.head_seq);
        record.alt_mode = alt;
        // Re-captured from the keeper's list so a port scan survives a restart.
        record.child_pid = Some(child_pid);
        record.sb_origin_pin = Some(pin_for_adoption(
            self.clock.mono_ns() / 1_000_000,
            applied.cols,
            applied.rows,
            history.evicted(),
            record.terminal_core.discarded_line_count().unwrap_or(0),
            record.terminal_core.scrollback_count() as u64,
        ));
        Ok(record)
    }
}

/// Replay a survivor's records into a COLD core, in order. Under eviction the
/// cold core's FIRST output write starts at an arbitrary cut, so its orphan
/// prefix is dropped (the ring keeps every byte); every later record continues
/// a warm parser and is replayed verbatim.
fn replay_ordered(core: &mut AlacrittyCore, history: &SurvivorHistory) -> Result<(), String> {
    let mut drop_orphan_prefix = history.evicted();
    for record in &history.records {
        match record {
            HistoryRecord::Output { bytes } => {
                let from = if drop_orphan_prefix {
                    skip_orphan_sequence_prefix(bytes)
                } else {
                    0
                };
                core.write(&bytes[from..]);
                drop_orphan_prefix = false;
            }
            HistoryRecord::Resize { cols, rows, .. } => {
                if !geometry_ok(*cols, *rows) {
                    return Err("keeper history contains invalid resize geometry".to_owned());
                }
                core.resize(*cols, *rows);
                if core.cols() != *cols || core.rows() != *rows {
                    return Err(
                        "terminal core did not retain keeper history resize geometry".to_owned(),
                    );
                }
            }
        }
    }
    Ok(())
}

fn geometry_ok(cols: u16, rows: u16) -> bool {
    is_terminal_geometry(&TerminalGeometry {
        cols: u32::from(cols),
        rows: u32::from(rows),
    })
}
