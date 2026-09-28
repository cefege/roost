//! The three bounds on a queued sparse batch — 64 frames, 250 appended
//! scrollback rows, 65536 spans — ported from
//! `apps/web/tests/terminalRenderScheduler.test.ts`.
//!
//! Each bound is driven on its own: a batch that would cross it is replaced by
//! the canonical full, so the pane converges and the DOM work is one re-render
//! instead of an unbounded queue.

mod render_scheduler_support;

use render_scheduler_support::{
    RecordingRenderer, delta_frame, flush_frame, full_frame, offer, row_shell, row_shell_of,
};
use roost_web_terminal::scheduler::{ApplyMode, EnqueueDecision, RenderScheduler};

#[test]
fn repairs_from_canonical_full_when_a_pending_batch_exceeds_a_bound() {
    let mut frame_bound = RenderScheduler::new();
    let mut frame_bound_renderer = RecordingRenderer::default();
    frame_bound.set_foreground(true);
    let frame_bound_baseline = full_frame(1, "A");
    offer(
        &mut frame_bound,
        &frame_bound_baseline,
        frame_bound_baseline.clone(),
        0,
    );
    flush_frame(&mut frame_bound, &mut frame_bound_renderer, 4);
    for seq in 2..=66u64 {
        let text = seq.to_string();
        offer(
            &mut frame_bound,
            &delta_frame(seq, &text),
            full_frame(seq, &text),
            seq * 10,
        );
    }
    let frame_bound_repair = flush_frame(&mut frame_bound, &mut frame_bound_renderer, 700);
    assert_eq!(frame_bound_repair.mode, Some(ApplyMode::FallbackFull));
    assert_eq!(
        frame_bound_repair.batch_frames,
        Some(65),
        "64 deltas rode and the 65th replaced the lot"
    );
    assert_eq!(frame_bound_renderer.full_seqs(), vec![1, 66]);
    assert!(frame_bound_renderer.delta_seqs().is_empty());

    let mut history_bound = RenderScheduler::new();
    let mut history_bound_renderer = RecordingRenderer::default();
    history_bound.set_foreground(true);
    let history_bound_baseline = full_frame(1, "A");
    offer(
        &mut history_bound,
        &history_bound_baseline,
        history_bound_baseline.clone(),
        0,
    );
    flush_frame(&mut history_bound, &mut history_bound_renderer, 4);
    let mut too_much_history = delta_frame(2, "B");
    too_much_history.scrollback_append = (0..251u32).map(|index| row_shell(index, &[])).collect();
    too_much_history.scrollback_total = 251;
    let too_much = offer(&mut history_bound, &too_much_history, full_frame(2, "B"), 8);
    assert_eq!(
        too_much.decision,
        EnqueueDecision::Coalesced {
            mode: ApplyMode::FallbackFull,
            batch_frames: 1,
            appended_rows: 251,
        }
    );
    flush_frame(&mut history_bound, &mut history_bound_renderer, 12);
    assert_eq!(history_bound_renderer.full_seqs(), vec![1, 2]);
    assert!(history_bound_renderer.delta_seqs().is_empty());

    let mut span_bound = RenderScheduler::new();
    let mut span_bound_renderer = RecordingRenderer::default();
    span_bound.set_foreground(true);
    let span_bound_baseline = full_frame(1, "A");
    offer(
        &mut span_bound,
        &span_bound_baseline,
        span_bound_baseline.clone(),
        0,
    );
    flush_frame(&mut span_bound, &mut span_bound_renderer, 4);
    let mut too_many_spans = delta_frame(2, "B");
    // One span past the bound is enough; the counter stops as soon as the limit
    // is passed, so a real paste never gets fully counted.
    too_many_spans.viewport_rows = vec![row_shell_of(0, 65_537, "x")];
    offer(&mut span_bound, &too_many_spans, full_frame(2, "B"), 8);
    flush_frame(&mut span_bound, &mut span_bound_renderer, 12);
    assert_eq!(span_bound_renderer.full_seqs(), vec![1, 2]);
    assert!(span_bound_renderer.delta_seqs().is_empty());
}
