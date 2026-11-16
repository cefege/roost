//! Replaying the keeper's ordered history into a cold terminal core, and the
//! geometry check both re-proof paths share. Called by `session::core_reprove`;
//! depends on `session::replay_align` and `roost-term`.

use roost_keeper::history::HistoryRecord;
use roost_protocol::viewport::{TerminalGeometry, is_terminal_geometry};
use roost_term::{RioCore, TerminalCore};

use super::keeper_channels::SurvivorHistory;
use super::replay_align::skip_orphan_sequence_prefix;

/// The rebuilt window and what the replay did.
pub(super) struct HistoryReplay {
    pub(super) window: Vec<u8>,
    pub(super) head_seq: u64,
    pub(super) replayed_bytes: usize,
    pub(super) tail_bytes: usize,
    pub(super) history_evicted: bool,
}

/// Replay the keeper's records into a cold core, marker by marker, then splice
/// on whatever this worker retained past the keeper's head. Synchronous by
/// contract: the caller holds the ring.
pub(super) fn replay_history(
    core: &mut RioCore,
    history: &SurvivorHistory,
    retained: &[u8],
    live_head_seq: u64,
) -> Result<HistoryReplay, String> {
    let mut window = Vec::new();
    let history_evicted = history.evicted();
    // Only the cold core's FIRST write under eviction can open mid-sequence.
    let mut drop_orphan_prefix = history_evicted;
    for entry in &history.records {
        match entry {
            HistoryRecord::Output { bytes, .. } => {
                window.extend_from_slice(bytes);
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
                if (core.cols(), core.rows()) != (*cols, *rows) {
                    return Err("terminal core did not retain a keeper history resize".to_owned());
                }
            }
        }
    }
    let replayed_bytes = window.len();
    let tail_bytes = live_head_seq.saturating_sub(history.head_seq) as usize;
    if tail_bytes > 0 {
        if tail_bytes > retained.len() {
            return Err("captured tail was evicted before the core could be re-proved".to_owned());
        }
        let tail = &retained[retained.len() - tail_bytes..];
        window.extend_from_slice(tail);
        core.write(tail);
    }
    Ok(HistoryReplay {
        window,
        // The keeper's ordered read may be ahead of this worker's head, and
        // the rebuilt window ends at whichever head reaches further.
        head_seq: history.head_seq.max(live_head_seq),
        replayed_bytes,
        tail_bytes,
        history_evicted,
    })
}

pub(super) fn geometry_ok(cols: u16, rows: u16) -> bool {
    is_terminal_geometry(&TerminalGeometry {
        cols: u32::from(cols),
        rows: u32::from(rows),
    })
}
