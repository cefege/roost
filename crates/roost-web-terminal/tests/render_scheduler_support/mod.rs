//! Shared fixtures for the render-scheduler tests: the v2 harness's frames, a
//! recording renderer, and one simulated browser frame.
//!
//! The frames are assembled directly rather than folded, so a failure names the
//! rule that broke instead of a fold that produced a different grid. Mirrors
//! `apps/web/tests/helpers/terminalRenderSchedulerHarness.ts`, with the
//! browser's own clock replaced by a caller-supplied `now_ms`.
#![allow(dead_code)]

use std::sync::Arc;

use roost_protocol::cell::{CellGridFrame, CellRow, CellSpan, MouseTracking};
use roost_web_terminal::scheduler::{
    ApplyMode, Delivery, EnqueueDecision, FrameDecision, PaintOutcome, RenderScheduler,
};

/// One cell carrying `text`, in the v2 harness's default colour pair.
pub fn span(text: &str) -> CellSpan {
    CellSpan {
        text: text.to_string(),
        fg: 256,
        bg: 256,
        flags: 0,
        fg_rgb: None,
        bg_rgb: None,
        columns: 1,
        link_uri: None,
        link_key: None,
    }
}

/// A row shell numbered `index`, carrying one span per cell. An empty cell list
/// builds the blank row the scrollback-append fixtures need.
pub fn row_shell(index: u32, cells: &[&str]) -> CellRow {
    let spans: Vec<CellSpan> = cells.iter().map(|cell| span(cell)).collect();
    CellRow {
        index,
        mark: 0,
        spans: Arc::from(spans),
    }
}

/// A row shell numbered `index` carrying `count` cells of `text`. A real grid
/// row is at most a few hundred cells wide; the span-bound fixture needs one
/// wider than any.
pub fn row_shell_of(index: u32, count: usize, text: &str) -> CellRow {
    let spans: Vec<CellSpan> = (0..count).map(|_| span(text)).collect();
    CellRow {
        index,
        mark: 0,
        spans: Arc::from(spans),
    }
}

/// A one-row authoritative full at `seq`, exactly as the v2 harness builds one.
pub fn full_frame(seq: u64, text: &str) -> CellGridFrame {
    CellGridFrame {
        stream_id: "stream-a".to_string(),
        grid_epoch: "epoch-a".to_string(),
        cols: 1,
        rows: 1,
        cursor_row: 0,
        cursor_col: 0,
        cursor_visible: true,
        alt_screen: false,
        cursor_keys_app: false,
        bracketed_paste: false,
        mouse_tracking: MouseTracking::None,
        mouse_sgr: false,
        focus_events: false,
        kitty_keyboard_flags: 0,
        full: true,
        viewport_rows: vec![row_shell(0, &[text])],
        scrollback_rows: Vec::new(),
        scrollback_append: Vec::new(),
        scrollback_total: 0,
        sb_base: 0,
        base_seq: 0,
        seq,
    }
}

/// A sparse delta extending the frame at `seq - 1`.
pub fn delta_frame(seq: u64, text: &str) -> CellGridFrame {
    CellGridFrame {
        full: false,
        base_seq: seq.saturating_sub(1),
        ..full_frame(seq, text)
    }
}

/// The renderer the v2 harness records with: full frames and delta batches kept
/// apart, in paint order.
#[derive(Debug, Default)]
pub struct RecordingRenderer {
    pub full_frames: Vec<CellGridFrame>,
    pub delta_batches: Vec<Vec<CellGridFrame>>,
    /// The verdict the next apply answers. The v2 harness always answered true;
    /// a refused paint is opt-in so the repair path can be driven.
    pub refuse_next: bool,
}

impl RecordingRenderer {
    /// The sequence numbers of the full frames painted, in paint order.
    pub fn full_seqs(&self) -> Vec<u64> {
        self.full_frames.iter().map(|frame| frame.seq).collect()
    }

    /// The sequence numbers of every delta batch, batch by batch.
    pub fn delta_seqs(&self) -> Vec<Vec<u64>> {
        self.delta_batches
            .iter()
            .map(|batch| batch.iter().map(|frame| frame.seq).collect())
            .collect()
    }

    fn verdict(&mut self) -> bool {
        let refused = self.refuse_next;
        self.refuse_next = false;
        !refused
    }

    pub fn apply_full(&mut self, frame: &CellGridFrame) -> bool {
        if !self.verdict() {
            return false;
        }
        self.full_frames.push(frame.clone());
        true
    }

    pub fn apply_deltas(&mut self, deltas: &[CellGridFrame]) -> bool {
        if !self.verdict() {
            return false;
        }
        self.delta_batches.push(deltas.to_vec());
        true
    }
}

/// The delivery facts, copied out so a test can hold them after the scheduler
/// borrow ends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedDelivery {
    pub frame: CellGridFrame,
    pub canonical: CellGridFrame,
    pub scrollback_appended: bool,
    pub had_wire_full: bool,
}

impl RecordedDelivery {
    /// The seven fields the v2 harness compared a delivery by.
    pub fn summary(&self) -> (u64, bool, bool, bool, u64, bool, bool) {
        (
            self.frame.seq,
            self.frame.full,
            self.frame.cursor_keys_app,
            self.frame.bracketed_paste,
            self.canonical.seq,
            self.canonical.full,
            self.scrollback_appended,
        )
    }
}

impl From<Delivery<'_>> for RecordedDelivery {
    fn from(delivery: Delivery<'_>) -> Self {
        Self {
            frame: delivery.frame.clone(),
            canonical: delivery.canonical.clone(),
            scrollback_appended: delivery.scrollback_appended,
            had_wire_full: delivery.had_wire_full,
        }
    }
}

/// What one simulated browser frame did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlushOutcome {
    /// Whether a batch reached the renderer at all.
    pub painted: bool,
    pub mode: Option<ApplyMode>,
    pub batch_frames: Option<u32>,
    pub queue_delay_ms: Option<u64>,
    pub delivery: Option<RecordedDelivery>,
}

impl FlushOutcome {
    fn nothing_painted() -> Self {
        Self {
            painted: false,
            mode: None,
            batch_frames: None,
            queue_delay_ms: None,
            delivery: None,
        }
    }
}

/// Run one browser frame: ask the scheduler what is due, hand the batch to the
/// recorder, and answer with the recorder's verdict.
///
/// It is the v2 harness's `flushNextAnimationFrame` plus its
/// `RecordingRenderer`.
pub fn flush_frame(
    scheduler: &mut RenderScheduler,
    renderer: &mut RecordingRenderer,
    now_ms: u64,
) -> FlushOutcome {
    let mut outcome = FlushOutcome::nothing_painted();
    let mut verdict = None;
    {
        let decision = scheduler.on_frame_fired(now_ms);
        if let FrameDecision::PaintNow(request) = decision {
            outcome.painted = true;
            outcome.mode = Some(request.mode);
            outcome.batch_frames = Some(request.batch_frames);
            outcome.queue_delay_ms = Some(request.queue_delay_ms);
            outcome.delivery = Some(request.delivery().into());
            verdict = Some(match request.mode {
                ApplyMode::DeltaBatch => renderer.apply_deltas(request.deltas),
                ApplyMode::WireFull | ApplyMode::FallbackFull => {
                    renderer.apply_full(request.canonical)
                }
            });
        }
    }
    // Only a paint that actually happened is answered. A parked or idle frame
    // left the batch in the slot, and the renderer never saw it.
    if let Some(verdict) = verdict {
        scheduler.complete_paint(if verdict {
            PaintOutcome::Applied
        } else {
            PaintOutcome::Refused
        });
    }
    outcome
}

/// What one arrival armed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Offer {
    pub decision: EnqueueDecision,
    /// Whether this arrival asked the host for a browser frame.
    pub armed: bool,
}

/// Offer a frame and arm whatever the scheduler then owes: the v2 `enqueue`
/// plus its `schedule()`, in one call.
///
/// A refused arrival arms nothing, because the v2 `enqueue` returns before it
/// reaches `schedule()`.
pub fn offer(
    scheduler: &mut RenderScheduler,
    frame: &CellGridFrame,
    canonical: CellGridFrame,
    now_ms: u64,
) -> Offer {
    let decision = scheduler.enqueue(frame, canonical, now_ms);
    let refused = matches!(
        decision,
        EnqueueDecision::RefusedStaleFull | EnqueueDecision::RefusedDisposed
    );
    Offer {
        decision,
        armed: !refused && scheduler.schedule_browser_frame(),
    }
}
