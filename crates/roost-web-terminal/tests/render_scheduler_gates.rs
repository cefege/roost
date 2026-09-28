//! The scheduler's own decisions beyond the v2 cases, against
//! `crates/roost-web-terminal/src/scheduler.rs` (the port of
//! `apps/web/src/renderer/terminal-render-scheduler.ts`): coalescing inside one
//! browser frame, the refused-paint repair, the wait a batch reports, exact
//! seq continuity, and a caller's answer with no paint outstanding.

mod render_scheduler_support;

use render_scheduler_support::{RecordingRenderer, delta_frame, flush_frame, full_frame, offer};
use roost_web_terminal::scheduler::{
    ApplyMode, EnqueueDecision, FrameDecision, PaintOutcome, RenderScheduler,
};

#[test]
fn two_frames_inside_one_browser_frame_produce_one_paint() {
    let mut scheduler = RenderScheduler::new();
    let mut renderer = RecordingRenderer::default();
    scheduler.set_foreground(true);
    let baseline = full_frame(1, "A");
    offer(&mut scheduler, &baseline, baseline.clone(), 0);
    flush_frame(&mut scheduler, &mut renderer, 4);

    let first = offer(&mut scheduler, &delta_frame(2, "B"), full_frame(2, "B"), 8);
    let second = offer(&mut scheduler, &delta_frame(3, "C"), full_frame(3, "C"), 10);
    assert!(
        first.armed,
        "the arrival that opens a batch is the one that arms"
    );
    assert!(
        !second.armed,
        "a second arrival inside the armed frame must not ask for a second frame"
    );
    assert_eq!(
        second.decision,
        EnqueueDecision::Coalesced {
            mode: ApplyMode::DeltaBatch,
            batch_frames: 2,
            appended_rows: 0,
        }
    );

    let paint = flush_frame(&mut scheduler, &mut renderer, 16);
    assert_eq!(paint.mode, Some(ApplyMode::DeltaBatch));
    assert_eq!(paint.batch_frames, Some(2));
    assert_eq!(
        paint.queue_delay_ms,
        Some(8),
        "a batch's wait is measured from the arrival that OPENED it"
    );
    assert_eq!(
        renderer.delta_seqs(),
        vec![vec![2, 3]],
        "two arrivals, one paint, one batch of two"
    );
    assert_eq!(renderer.full_seqs(), vec![1]);
}

#[test]
fn a_refused_paint_is_repaired_as_a_fallback_full_and_keeps_its_queue_clock() {
    let mut scheduler = RenderScheduler::new();
    let mut renderer = RecordingRenderer::default();
    scheduler.set_foreground(true);
    let baseline = full_frame(1, "A");
    offer(&mut scheduler, &baseline, baseline.clone(), 0);
    flush_frame(&mut scheduler, &mut renderer, 4);
    offer(&mut scheduler, &delta_frame(2, "B"), full_frame(2, "B"), 8);

    let applied = scheduler.reconciled_watermark();
    assert!(
        applied.is_some(),
        "the baseline painted, so the watermark already names its canonical"
    );
    renderer.refuse_next = true;
    let refused = flush_frame(&mut scheduler, &mut renderer, 12);
    assert!(refused.painted, "the batch WAS handed to the renderer");
    assert!(renderer.delta_seqs().is_empty());
    assert_eq!(
        scheduler.pending_mode(),
        Some(ApplyMode::FallbackFull),
        "a refused delta batch is repaired as the canonical full it named"
    );
    assert_eq!(
        scheduler.reconciled_watermark(),
        applied,
        "a refused paint must not advance the watermark the next delta extends"
    );
    assert!(
        scheduler.needs_browser_frame(),
        "and the repaired batch still owes a paint"
    );

    let repair = flush_frame(&mut scheduler, &mut renderer, 16);
    assert_eq!(repair.mode, Some(ApplyMode::FallbackFull));
    assert_eq!(
        repair.batch_frames,
        Some(1),
        "the arrival count carries over: the baseline painted and emptied the \
         slot, so this batch is the one delta that was refused"
    );
    assert_eq!(
        repair.queue_delay_ms,
        Some(8),
        "so does the queue clock: the repair is the same batch, waited on longer"
    );
    assert_eq!(
        renderer.full_seqs(),
        vec![1, 2],
        "the baseline painted its own canonical, and the repair paints the \
         refused batch's"
    );
    assert_eq!(
        scheduler.reconciled_watermark().and_then(|mark| mark.seq),
        Some(2)
    );
    assert!(
        !repair.delivery.expect("the repair delivered").had_wire_full,
        "a repaired delta batch descends from no wire full"
    );
}

#[test]
fn a_batch_reports_exactly_the_wait_the_caller_supplied() {
    // The same frame sequence three times over, differing only in the clock the
    // caller hands over. Identical decisions, and a wait that is the caller's
    // own arithmetic and nothing else — including at the zero-wait boundary.
    for (queued_at_ms, fired_at_ms) in [(0u64, 4u64), (1_000, 1_007), (5_000, 5_000)] {
        let mut scheduler = RenderScheduler::new();
        let mut renderer = RecordingRenderer::default();
        scheduler.set_foreground(true);
        let baseline = full_frame(1, "A");
        offer(&mut scheduler, &baseline, baseline.clone(), queued_at_ms);
        let base_paint = flush_frame(&mut scheduler, &mut renderer, fired_at_ms);
        offer(
            &mut scheduler,
            &delta_frame(2, "B"),
            full_frame(2, "B"),
            queued_at_ms.saturating_add(1),
        );
        let paint = flush_frame(&mut scheduler, &mut renderer, queued_at_ms + 9);

        assert_eq!(base_paint.mode, Some(ApplyMode::WireFull));
        assert_eq!(paint.mode, Some(ApplyMode::DeltaBatch));
        assert_eq!(paint.queue_delay_ms, Some(8));
        assert_eq!(renderer.full_seqs(), vec![1]);
        assert_eq!(renderer.delta_seqs(), vec![vec![2]]);
    }
}

#[test]
fn a_delta_whose_seq_does_not_follow_its_own_base_repairs_from_the_canonical() {
    let mut scheduler = RenderScheduler::new();
    let mut renderer = RecordingRenderer::default();
    scheduler.set_foreground(true);
    let baseline = full_frame(1, "A");
    offer(&mut scheduler, &baseline, baseline.clone(), 0);
    flush_frame(&mut scheduler, &mut renderer, 4);

    // Its base IS the painted watermark, but it claims to reach two sequences
    // at once: folding it would paint rows it was never diffed to produce.
    let mut skipping = delta_frame(3, "C");
    skipping.base_seq = 1;
    assert_eq!(
        offer(&mut scheduler, &skipping, full_frame(3, "C"), 8).decision,
        EnqueueDecision::Coalesced {
            mode: ApplyMode::FallbackFull,
            batch_frames: 1,
            appended_rows: 0,
        },
        "v2 admits a delta only when seq === baseSeq + 1"
    );
    flush_frame(&mut scheduler, &mut renderer, 12);
    assert_eq!(renderer.full_seqs(), vec![1, 3]);
    assert!(renderer.delta_seqs().is_empty());

    // At the top of the sequence space "base + 1" must not saturate into a
    // match with a frame that names its own base as its sequence.
    let mut top = RenderScheduler::new();
    let mut top_renderer = RecordingRenderer::default();
    top.set_foreground(true);
    let top_baseline = full_frame(u64::MAX, "A");
    offer(&mut top, &top_baseline, top_baseline.clone(), 0);
    flush_frame(&mut top, &mut top_renderer, 4);
    let mut stuck = delta_frame(u64::MAX, "B");
    stuck.base_seq = u64::MAX;
    assert_eq!(
        offer(&mut top, &stuck, full_frame(u64::MAX, "B"), 8).decision,
        EnqueueDecision::Coalesced {
            mode: ApplyMode::FallbackFull,
            batch_frames: 1,
            appended_rows: 0,
        }
    );
}

#[test]
fn an_answer_with_no_paint_outstanding_changes_nothing() {
    let mut scheduler = RenderScheduler::new();
    let mut renderer = RecordingRenderer::default();
    scheduler.set_foreground(true);
    let baseline = full_frame(1, "A");
    offer(&mut scheduler, &baseline, baseline.clone(), 0);
    flush_frame(&mut scheduler, &mut renderer, 4);
    offer(&mut scheduler, &delta_frame(2, "B"), full_frame(2, "B"), 8);

    scheduler.set_foreground(false);
    assert_eq!(scheduler.on_frame_fired(10), FrameDecision::Parked);
    scheduler.complete_paint(PaintOutcome::Applied);
    assert_eq!(
        scheduler.pending_mode(),
        Some(ApplyMode::FallbackFull),
        "a parked frame painted nothing, so answering it must not consume the batch"
    );
    assert_eq!(
        scheduler.reconciled_watermark().and_then(|mark| mark.seq),
        Some(1),
        "nor advance the watermark past a DOM that was never touched"
    );

    scheduler.set_foreground(true);
    let resumed = flush_frame(&mut scheduler, &mut renderer, 20);
    assert_eq!(resumed.mode, Some(ApplyMode::FallbackFull));
    assert_eq!(renderer.full_seqs(), vec![1, 2]);

    offer(&mut scheduler, &delta_frame(3, "C"), full_frame(3, "C"), 24);
    scheduler.complete_paint(PaintOutcome::Applied);
    assert_eq!(
        scheduler.pending_mode(),
        Some(ApplyMode::DeltaBatch),
        "a second answer to one paint must not swallow the batch that arrived after it"
    );
    assert_eq!(
        scheduler.reconciled_watermark().and_then(|mark| mark.seq),
        Some(2)
    );
    flush_frame(&mut scheduler, &mut renderer, 28);
    assert_eq!(renderer.delta_seqs(), vec![vec![3]]);
}
