//! Per-replica counts of decoded wire frames: how many arrived, how many were
//! complete fulls, the history rows the last full carried, and the latest grid
//! epoch. Written by `TerminalSession::admit_decoded`; read by roost-web's smoke
//! backdoor (`cellFrameCount`, `cellFullFrameCount`, `lastFullFrameSbRows`,
//! `cellGridEpoch`). Ports the `noteWireFrame` counters of
//! `apps/web/src/store/terminal-stream-replica.ts:252-270`.

use roost_protocol::cell::CellGridFrame;

/// What one replica has received, counted where v2 counts it: after the frame
/// passed the generation and stream fences and decoded, before the fold judged it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FrameCounts {
    frames: u64,
    full_frames: u64,
    last_full_scrollback_rows: Option<usize>,
    grid_epoch: String,
}

impl FrameCounts {
    /// Count one decoded frame. An assembled chunked baseline counts once.
    pub fn note_decoded(&mut self, frame: &CellGridFrame) {
        self.frames += 1;
        self.grid_epoch.clone_from(&frame.grid_epoch);
        if frame.full {
            self.full_frames += 1;
            self.last_full_scrollback_rows = Some(frame.scrollback_rows.len());
        }
    }

    /// Every decoded frame, full or delta.
    pub fn frames(&self) -> u64 {
        self.frames
    }

    /// Complete baselines.
    pub fn full_frames(&self) -> u64 {
        self.full_frames
    }

    /// History rows the latest full carried, or `-1` before any full (v2's
    /// sentinel, which the oracle compares against).
    pub fn last_full_scrollback_rows(&self) -> i64 {
        self.last_full_scrollback_rows
            .map_or(-1, |rows| i64::try_from(rows).unwrap_or(i64::MAX))
    }

    /// The grid epoch on the latest frame, empty before any.
    pub fn grid_epoch(&self) -> &str {
        &self.grid_epoch
    }
}
