//! The replica→renderer delivery for one pane. The store holds the session's
//! canonical frame and a per-session `frame_revision`; the pane paints that
//! canonical on the browser frame after the revision moved, so every arrival
//! inside one browser frame costs one paint. Target-independent; driven by the
//! wasm pane mount. Replaces the subscriber half of v2's
//! `apps/web/src/components/terminal/cell-terminal-renderer.ts`
//! (`view.subscribeRenderer`) and the coalescing of
//! `apps/web/src/renderer/terminal-render-scheduler.ts`.

use roost_protocol::cell::CellGridFrame;

/// The facts that say two canonicals are the same grid: a delta may only ever
/// extend a grid with all five equal.
#[derive(Debug, Clone, PartialEq, Eq)]
struct GridKey {
    stream_id: String,
    grid_epoch: String,
    cols: u32,
    rows: u32,
    alt_screen: bool,
}

impl GridKey {
    fn of(frame: &CellGridFrame) -> Self {
        Self {
            stream_id: frame.stream_id.clone(),
            grid_epoch: frame.grid_epoch.clone(),
            cols: frame.cols,
            rows: frame.rows,
            alt_screen: frame.alt_screen,
        }
    }
}

/// What one paint delivered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Delivery {
    /// The painted canonical is a different grid from the last one: a new
    /// baseline, not output. Presentation counts only continuations as
    /// activity, and the pager re-anchors only on a baseline.
    pub baseline: bool,
}

/// One pane's delivery watermark.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FrameFeed {
    seen_revision: Option<u64>,
    paint_owed: bool,
    painted: Option<GridKey>,
}

impl FrameFeed {
    /// Nothing seen, nothing painted.
    pub fn new() -> Self {
        Self::default()
    }

    /// The store moved. Answers true when the replica advanced, so the caller
    /// asks for a browser frame. The first observation of a replica that
    /// already holds frames owes a paint too: a remounted pane paints the
    /// canonical it finds.
    pub fn observe_revision(&mut self, frame_revision: u64) -> bool {
        if self.seen_revision == Some(frame_revision) {
            return false;
        }
        self.seen_revision = Some(frame_revision);
        if frame_revision == 0 {
            return false;
        }
        self.paint_owed = true;
        true
    }

    /// Whether a paint is owed.
    pub fn paint_owed(&self) -> bool {
        self.paint_owed
    }

    /// Owe a paint again, e.g. after a refused paint or a skipped delivery.
    pub fn owe_paint(&mut self) {
        self.paint_owed = true;
    }

    /// The canonical was painted.
    pub fn painted(&mut self, canonical: &CellGridFrame) -> Delivery {
        self.paint_owed = false;
        let key = GridKey::of(canonical);
        let baseline = self.painted.as_ref() != Some(&key);
        if baseline {
            tracing::debug!(target: "terminal", stream_id = %key.stream_id,
                grid_epoch = %key.grid_epoch, seq = canonical.seq, "pane painted a new baseline");
        }
        self.painted = Some(key);
        Delivery { baseline }
    }

    /// Skip the owed delivery without painting it; the next revision paints
    /// the newer canonical, which is what a dropped delta repairs to.
    pub fn skip(&mut self) {
        self.paint_owed = false;
    }
}
