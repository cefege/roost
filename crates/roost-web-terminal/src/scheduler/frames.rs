//! The batch one terminal renderer's browser frame carries — v2's
//! `PendingTerminalRender` and `fullPending` in
//! `apps/web/src/renderer/terminal-render-scheduler.ts`: the sparse deltas
//! waiting to fold, the canonical frame that stands in for them, and the
//! delivery a landed paint publishes. Owned by `RenderScheduler`; the rules
//! that shape a batch are `scheduler::frame_gate`.

use roost_protocol::cell::CellGridFrame;

use super::frame_gate::own_queued_delta;

/// How one paint reaches the renderer.
///
/// `RendererFrameMode` names the incident phase a repair is about and has two
/// members. A DELIVERY has three, because standing in for a batch that could
/// not be delivered as a delta is a third delivery — and it is what makes
/// `had_wire_full` on a delivery a fact rather than a duplicate of the mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplyMode {
    /// A `full` that arrived on the wire, applied as the canonical frame it
    /// names.
    WireFull,
    /// The canonical frame standing in for a batch that could not be delivered
    /// as a delta: a sequence gap, a different grid, a bound, or a paint the
    /// renderer refused.
    FallbackFull,
    /// One or more sparse deltas, folded in arrival order.
    DeltaBatch,
}

/// The frame a delivery names for a batch: its last delta, or — a batch that
/// carries none, which only a hand-built one can be — its canonical frame.
/// Naming the rule ONCE is what keeps a delivery and a downgrade agreeing.
fn delivery_of<'a>(deltas: &'a [CellGridFrame], canonical: &'a CellGridFrame) -> &'a CellGridFrame {
    deltas.last().unwrap_or(canonical)
}

/// The batch waiting for its browser frame, in one of exactly two shapes.
///
/// The split is load-bearing: a `Delta` batch holds the frames themselves and a
/// `Full` holds the canonical frame plus the ONE frame a delivery names. Every
/// mutation is a whole-batch operation — append a delta, or stand the canonical
/// in for the lot — because a batch that was partly a delta and partly a full
/// would be a grid no frame ever described.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum PendingRender {
    /// A run of contiguous sparse deltas.
    Delta {
        /// The newest canonical frame the run folds onto. It advances with every
        /// delta, so a paint always reconciles to the LAST arrival rather than
        /// the first.
        canonical: CellGridFrame,
        deltas: Vec<CellGridFrame>,
        /// Scrollback lines the run appends in total.
        appended_rows: u64,
        /// Spans the run carries, against the batch span bound.
        span_count: usize,
        queued_at_ms: u64,
    },
    /// The canonical frame, applied wholesale.
    Full {
        canonical: CellGridFrame,
        /// The frame a delivery names: the wire full this batch stands on, or
        /// the last delta the batch replaced.
        delivery: CellGridFrame,
        /// Only ever `WireFull` or `FallbackFull`; `DeltaBatch` is what a
        /// `Delta` batch applies as.
        source: ApplyMode,
        appended_rows: u64,
        /// How many arrivals this batch carries, including the one that made it
        /// a full.
        batch_frames: u32,
        /// Whether a `full` that arrived on the wire is anywhere in its history.
        had_wire_full: bool,
        queued_at_ms: u64,
    },
}

impl PendingRender {
    /// A batch of one authoritative full.
    pub(super) fn full(
        canonical: CellGridFrame,
        delivery: CellGridFrame,
        source: ApplyMode,
        appended_rows: u64,
        batch_frames: u32,
        had_wire_full: bool,
        queued_at_ms: u64,
    ) -> Self {
        Self::Full {
            canonical,
            delivery,
            source,
            appended_rows,
            batch_frames,
            had_wire_full,
            queued_at_ms,
        }
    }

    /// The canonical frame standing in for a batch that could not be delivered
    /// as a delta. `previous` is whatever the slot held — nothing, a delta run
    /// or a full — and its arrival count, its wire-full history and its queue
    /// clock carry into the replacement, while the frame that could not ride
    /// becomes the frame a delivery names.
    pub(super) fn fallback_full(
        previous: Option<Self>,
        frame: &CellGridFrame,
        canonical: CellGridFrame,
        now_ms: u64,
    ) -> Self {
        let appended = u64::try_from(frame.scrollback_append.len()).unwrap_or(u64::MAX);
        let (appended_rows, batch_frames, had_wire_full, queued_at_ms) = match previous.as_ref() {
            Some(previous) => (
                previous.appended_rows().saturating_add(appended),
                previous.batch_frames().saturating_add(1),
                previous.had_wire_full(),
                previous.queued_at_ms(),
            ),
            None => (appended, 1, false, now_ms),
        };
        Self::full(
            canonical,
            own_queued_delta(frame),
            ApplyMode::FallbackFull,
            appended_rows,
            batch_frames,
            had_wire_full,
            queued_at_ms,
        )
    }

    /// A batch of one sparse delta, which may grow.
    pub(super) fn delta(
        canonical: CellGridFrame,
        delta: CellGridFrame,
        appended_rows: u64,
        span_count: usize,
        queued_at_ms: u64,
    ) -> Self {
        Self::Delta {
            canonical,
            deltas: vec![delta],
            appended_rows,
            span_count,
            queued_at_ms,
        }
    }

    /// Stand the canonical frame in for this batch.
    ///
    /// Used by the two paths that cannot deliver a delta batch: parking the
    /// pane, and a paint the renderer refused. Both keep the queue clock, the
    /// appended-row count and the arrival count, and both record that no wire
    /// full is in the batch's history — the batch descends from deltas only.
    pub(super) fn downgrade_to_fallback_full(self) -> Self {
        let queued_at_ms = self.queued_at_ms();
        let appended_rows = self.appended_rows();
        let had_wire_full = self.had_wire_full();
        match self {
            full @ Self::Full { .. } => full,
            Self::Delta {
                canonical, deltas, ..
            } => Self::full(
                canonical.clone(),
                delivery_of(&deltas, &canonical).clone(),
                ApplyMode::FallbackFull,
                appended_rows,
                u32::try_from(deltas.len()).unwrap_or(u32::MAX),
                had_wire_full,
                queued_at_ms,
            ),
        }
    }

    /// The canonical frame a paint reconciles to.
    pub(super) fn canonical(&self) -> &CellGridFrame {
        match self {
            Self::Delta { canonical, .. } | Self::Full { canonical, .. } => canonical,
        }
    }

    /// The frame a delivery names: the last delta, or the wire full.
    pub(super) fn delivery(&self) -> &CellGridFrame {
        match self {
            Self::Delta {
                canonical, deltas, ..
            } => delivery_of(deltas, canonical),
            Self::Full { delivery, .. } => delivery,
        }
    }

    /// The sparse deltas, oldest first; empty unless the batch applies as one.
    pub(super) fn deltas(&self) -> &[CellGridFrame] {
        match self {
            Self::Delta { deltas, .. } => deltas,
            Self::Full { .. } => &[],
        }
    }

    /// How this batch will be applied.
    pub(super) fn apply_mode(&self) -> ApplyMode {
        match self {
            Self::Delta { .. } => ApplyMode::DeltaBatch,
            Self::Full { source, .. } => *source,
        }
    }

    pub(super) fn appended_rows(&self) -> u64 {
        match self {
            Self::Delta { appended_rows, .. } | Self::Full { appended_rows, .. } => *appended_rows,
        }
    }

    pub(super) fn batch_frames(&self) -> u32 {
        match self {
            Self::Delta { deltas, .. } => u32::try_from(deltas.len()).unwrap_or(u32::MAX),
            Self::Full { batch_frames, .. } => *batch_frames,
        }
    }

    pub(super) fn had_wire_full(&self) -> bool {
        match self {
            Self::Delta { .. } => false,
            Self::Full { had_wire_full, .. } => *had_wire_full,
        }
    }

    pub(super) fn queued_at_ms(&self) -> u64 {
        match self {
            Self::Delta { queued_at_ms, .. } | Self::Full { queued_at_ms, .. } => *queued_at_ms,
        }
    }
}

/// One batch, handed to the renderer for a single paint.
///
/// It borrows the scheduler's slot rather than moving out of it, because a
/// refused paint has to put the batch BACK: that is the whole repair path, and
/// copying a batch per frame to dodge the borrow would be the expensive way to
/// spell "it failed". The caller answers with `RenderScheduler::complete_paint`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaintRequest<'a> {
    /// How the batch must be applied.
    pub mode: ApplyMode,
    /// The canonical frame the DOM reconciles to.
    pub canonical: &'a CellGridFrame,
    /// The sparse deltas, oldest first. Empty unless `mode` is `DeltaBatch`.
    pub deltas: &'a [CellGridFrame],
    /// The frame a delivery names: the last delta, or the wire full this batch
    /// stands on. It is a DIFFERENT frame from `canonical` whenever the batch
    /// carries deltas, and that difference is the delivery.
    pub delivery: &'a CellGridFrame,
    /// Scrollback lines the batch appends, summed over its arrivals.
    pub appended_rows: u64,
    /// How many arrivals the batch carries.
    pub batch_frames: u32,
    /// Whether a `full` that arrived on the wire is in the batch's history.
    pub had_wire_full: bool,
    /// How long the batch waited for this browser frame, from the arrival that
    /// opened the batch to the frame that painted it.
    pub queue_delay_ms: u64,
}

impl PaintRequest<'_> {
    /// The delivery this batch publishes once it lands. It borrows the request,
    /// which the caller holds for exactly as long as it needs the facts.
    pub fn delivery(&self) -> Delivery<'_> {
        Delivery {
            frame: self.delivery,
            canonical: self.canonical,
            scrollback_appended: self.appended_rows > 0,
            had_wire_full: self.had_wire_full,
        }
    }
}

/// The facts a landed batch publishes to the front end.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Delivery<'a> {
    /// The frame the front end acts on: the last delta, or the wire full.
    pub frame: &'a CellGridFrame,
    /// The canonical frame it was delivered against.
    pub canonical: &'a CellGridFrame,
    /// Whether the batch appended scrollback lines. A viewport-only checkpoint
    /// can answer `true` while painting no history row at all, which is why
    /// this is a count and not a claim about the DOM.
    pub scrollback_appended: bool,
    /// Whether the batch descends from a `full` that arrived on the wire.
    pub had_wire_full: bool,
}

/// What the renderer answered when it was handed a batch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaintOutcome {
    /// The batch is in the DOM. The reconcile watermark advances to its
    /// canonical frame.
    Applied,
    /// The batch was refused: the fold failed, the base was gone, or the
    /// renderer's own repair could not complete. The batch is repaired as a
    /// fallback full and rides the next browser frame.
    Refused,
}
