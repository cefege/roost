//! What one replica delivers to its renderers: the frame revision, and every
//! accepted sparse delta since the last full, bounded as one pending render
//! batch is bounded. Written by `TerminalSession::admit_decoded`; read by
//! roost-web's pane paint loop, which folds the deltas a pane has not painted
//! so their appended history reaches the DOM. Ports the delta queue of
//! `apps/web/src/renderer/terminal-render-scheduler.ts` as a shared log.

use roost_protocol::cell::CellGridFrame;

/// Sparse deltas one batch may fold before a renderer takes the canonical full.
pub const MAX_PENDING_DELTA_FRAMES: usize = 64;
/// History rows one batch may append before a renderer takes the canonical full.
pub const MAX_PENDING_SCROLLBACK_ROWS: usize = 250;
/// Spans one batch may carry before a renderer takes the canonical full.
pub const MAX_PENDING_DELTA_SPANS: usize = 65_536;

/// The replica's revision counter and its retained delta suffix.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RendererDeliveries {
    /// How many frames have been APPLIED to the replica.
    ///
    /// A renderer repaints when this moved, and it is per session rather than
    /// one store-wide counter because the coordinator delivers a frame per pane
    /// per tick: on a four-pane board a store-wide counter repaints all four for
    /// each of the four. It moves only where the CANONICAL grid moves: a full
    /// that replaced the replica, and a delta that extended it. A refused
    /// frame, a chunk still assembling, and a dropped stalled partial change
    /// bookkeeping and no cell.
    revision: u64,
    /// The revision whose canonical `deltas[0]` extends.
    base_revision: u64,
    deltas: Vec<CellGridFrame>,
    appended_rows: usize,
    spans: usize,
}

impl RendererDeliveries {
    /// The current revision.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// A full replaced the replica: no delta reaches back across it.
    pub fn note_full(&mut self) {
        self.revision += 1;
        self.clear();
    }

    /// A delta extended the replica. The oldest deltas leave once the suffix
    /// outgrows one batch: a renderer that far behind takes the full anyway.
    pub fn note_delta(&mut self, delta: CellGridFrame) {
        self.revision += 1;
        let rows = delta.scrollback_append.len();
        let spans = delta_spans(&delta);
        if rows > MAX_PENDING_SCROLLBACK_ROWS || spans > MAX_PENDING_DELTA_SPANS {
            self.clear();
            return;
        }
        self.deltas.push(delta);
        self.appended_rows += rows;
        self.spans += spans;
        let mut evicted = 0;
        while self.deltas.len() - evicted > MAX_PENDING_DELTA_FRAMES
            || self.appended_rows > MAX_PENDING_SCROLLBACK_ROWS
            || self.spans > MAX_PENDING_DELTA_SPANS
        {
            let Some(oldest) = self.deltas.get(evicted) else {
                break;
            };
            self.appended_rows -= oldest.scrollback_append.len();
            self.spans -= delta_spans(oldest);
            evicted += 1;
        }
        self.deltas.drain(..evicted);
        self.base_revision += evicted as u64;
    }

    /// Every delta after `painted_revision`, oldest first: empty when nothing
    /// moved, `None` when the log no longer reaches back that far and the
    /// renderer must take the canonical full.
    pub fn deltas_since(&self, painted_revision: u64) -> Option<&[CellGridFrame]> {
        let offset = painted_revision.checked_sub(self.base_revision)?;
        let offset = usize::try_from(offset).ok()?;
        self.deltas.get(offset..)
    }

    fn clear(&mut self) {
        self.base_revision = self.revision;
        self.deltas.clear();
        self.appended_rows = 0;
        self.spans = 0;
    }
}

/// Spans a delta carries, viewport and appended history alike.
fn delta_spans(delta: &CellGridFrame) -> usize {
    delta
        .viewport_rows
        .iter()
        .chain(&delta.scrollback_append)
        .map(|row| row.spans.len())
        .sum()
}
